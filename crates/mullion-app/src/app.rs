//! App:winit ApplicationHandler<UserEvent>。持有窗口/GPU/文字层/运行时,以及一个
//! `Option<Workspace>`(launcher 态 None / 终端态 Some,§2.2;F30 起一个 Workspace
//! 可装多个 pane)。每帧(ws 存在时)对每个 pane「排空 rx → feed emu → 回写
//! PtyWrite(T1)」,GPU present 受帧率(T3)与同步块(T2)双闸。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mullion_core::layout::PaneId;
use mullion_ssh::config::SshConfig;
use mullion_ssh::known_hosts::HostKeyPolicy;
use mullion_ssh::session::{SshConnection, SshSession};
use mullion_store::known_hosts::KnownHostsFile;
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
use crate::shell::workspace::{PaneGeom, PaneState, Preset, Workspace};
use crate::text::TextLayer;
use crate::theme::{self, MULLION_DARK};
use crate::{diag, input, shell};

/// app 与「连接建立」异步任务之间的事件(ssh io_task / connect 的 wake、结果经此回送)。
/// 携带 `SshSession`/`Receiver` 等非 `Copy` 负载,故不能派生 Copy/Clone;两者也未实现
/// `Debug`,故 `UserEvent` 同样不派生 Debug(winit `ApplicationHandler<T>` 只要求 `T: 'static`)。
pub enum UserEvent {
    Wake,
    /// 异步 connect 成功:第一条 channel 的句柄 + 远端字节接收端(app 每帧 drain),
    /// 以及**已建立连接**本身的 `Handle`(F35:同一条连接上后续分屏另开 channel
    /// 要复用它)。`Arc` 是因为 russh 的 `Handle` 没实现 `Clone`,只有 `Drop`
    /// (释放即断连)。
    ConnectOk {
        ssh: SshSession,
        rx: Receiver<Vec<u8>>,
        handle: Arc<SshConnection>,
    },
    /// 异步 connect 失败,已格式化的可操作错误(F6 分类由 `session::connect` 内部给)。
    ConnectErr(String),
    /// 私钥文件对话框结束。`None` = 用户取消/对话框失败——也要回送,否则
    /// `key_picker_busy` 永远清不掉,以后再点「选择…」就没反应了。
    KeyPathPicked(Option<PathBuf>),
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
    /// F92:一次拨测成功。`u64` 是发起时的世代号,过期的直接丢。
    ProbeOk(u64),
    /// F92:一次拨测失败(含超时)。
    ProbeErr(u64, String),
    /// F40~F44:一次自动化结束。`u64` 是发起时的 `Workspace` 世代号,
    /// 过期的直接丢(同 `PaneOpenErr::generation`)。
    AutomationDone(u64, crate::automation::Outcome),
}

/// 一次在途自动化的把手。三条通道都是 `Option`,因为每一条都是**一次性边**:
/// `take()` 天然保证不会重复触发,也省掉一个「是否已触发」的布尔标志。
struct AutomationHandle {
    /// 只认这一个 pane 的首字节。总设计 §7 前提②:分屏新开的 pane 是干净
    /// shell,不重复跑自动化(所有 pane attach 同一个 tmux session 会内容
    /// 镜像,且 `window-size` 取 `latest` 会反复 reflow、取 `smallest` 会留白)。
    pane: PaneId,
    ready: Option<tokio::sync::oneshot::Sender<()>>,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
    disconnect: Option<tokio::sync::oneshot::Sender<()>>,
    /// 换新连接时 abort:旧那次的结论对新连接没有意义。
    task: tokio::task::JoinHandle<()>,
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
    /// `None` = launcher 态(无终端可画);`Some` = 终端态。取代原来的
    /// `Connection`:后者只能装一条连接 + 一个 pane。
    ws: Option<Workspace>,
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
    /// 当前生效的布局预设(工具栏画选中态用)。手动关 pane 之后置 `None`
    /// (布局不再对应任何预设)。
    current_preset: Option<Preset>,
    /// 最近一次发起连接用的配置。`open_pty`(F35 分屏复用连接)要它的
    /// `term`/`cols`/`rows`,标题条要 `user`/`host`/`port`。
    last_cfg: Option<SshConfig>,
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
    /// egui 侧有内容待画(菜单展开/hover/弹窗/错误提示)。与「终端来了新字节」是
    /// 两个独立脏源,`frame::frame_is_dirty` 取并集——只看终端字节的话,远端一安静
    /// egui 的交互就被 `RedrawAction::Idle` 吞掉,菜单点不开。
    ui_dirty: bool,
    /// 指针最近一次的物理像素坐标。`MouseWheel` 事件本身不带坐标,鼠标上报
    /// (F17 alt screen 档)要的 (col,row) 只能靠 `CursorMoved` 记着。
    cursor_px: (f32, f32),
    /// 系统剪贴板(F18)。打不开时内部退化为 no-op(见 `crate::clipboard`)。
    clipboard: crate::clipboard::Clipboard,
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
    /// 待用户确认的多行粘贴(F18)。`Some` = 弹窗开着,计入 `modal`(T8)。
    pending_paste: Option<String>,
    /// 当前已连接的会话(状态点用)。`ConnectOk` 时从 `ui.connect_request_last`
    /// 记下来,`UserEvent::ConnectOk` 本身不带 SessionId。
    connected_session: Option<mullion_store::SessionId>,
    /// F61/F62:会话外观的解析缓存。**只在会话/分组变更后 rebuild**,
    /// 绝不在渲染里现算(陷阱 T3,见 `ui::badge::AppearanceCache`)。
    appearance: crate::ui::badge::AppearanceCache,
    /// F92 拨测世代号。切会话 / 关编辑器 / 关会话管理器时 +1,
    /// 迟到的结果据此丢弃(见 `accept_probe`)。
    probe_epoch: u64,
    /// 在途拨测任务。退出或取消时 abort —— 20 秒的 timeout 悬着不管,
    /// 关窗后进程还要多活 20 秒。
    probe_task: Option<tokio::task::JoinHandle<()>>,
    /// F40~F44:正在跑的那一次自动化。`None` = 没在跑。
    automation: Option<AutomationHandle>,
    /// `spawn_connect` 算好、等 `ConnectOk` 抵达时启用的计划。
    ///
    /// 在 `spawn_connect`(用户点击那一帧)算而不是 `ConnectOk` 里算:
    /// `ConnectOk` 不携带 `SessionId`,到那时只能读 `ui.connect_request_last`,
    /// 而连接在途期间用户完全可能改了配置甚至删了这条会话。
    pending_automation: Option<crate::automation::PendingAutomation>,
    /// F44 右键「连接(跳过自动化)」的一次性标志。`ConnectOk` 消费后立即清零。
    pending_skip_automation: bool,
    /// 上一次自动化的结论文案。一直显示到下一次 `spawn_connect` 才清空 ——
    /// 不做定时淡出:状态栏本来就是常驻信息区,而定时清除需要再引一个
    /// deadline 进帧循环,正是 spec §1 修订一要避免的东西。
    automation_status: Option<String>,
}

/// 显示字号(磅 / point)。渲染时按窗口 DPI 缩放成物理像素。
/// TODO:与字体族一起做成可配置(见 spec F21)。
const FONT_POINT_SIZE: f32 = 10.0;

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
            ws: None,
            next_ws_generation: 0,
            start: Instant::now(),
            mods: ModifiersState::empty(),
            kitty: false, // MVP 未协商 Kitty,走优雅退化(T6)
            active: None,
            limiter: FrameLimiter::new(16), // ~60fps(T3)
            next_frame_at: None,
            proxy,
            known_hosts,
            pending_host_key: None,
            host_key_since: None,
            last_cfg: initial.clone(),
            initial,
            cli_direct,
            current_preset: Some(Preset::Single),
            ui: crate::ui::UiState::default(),
            store: None,
            visible: shell::window_state::Visibility::default(),
            key_picker_busy: false,
            ui_dirty: true, // 首帧必须画出来
            cursor_px: (0.0, 0.0),
            clipboard: crate::clipboard::Clipboard::new(),
            dragging: false,
            prev_click: None,
            press_anchor: None,
            autoscroll: 0,
            pending_paste: None,
            connected_session: None,
            appearance: Default::default(),
            probe_epoch: 0,
            probe_task: None,
            automation: None,
            pending_automation: None,
            pending_skip_automation: false,
            automation_status: None,
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
        sync_timeout_wake_at(self.start, self.ws.as_ref(), now_ms)
    }

    /// UI 侧变了(或 egui 自己要重绘):标脏 + 请求一帧。**两件事必须一起做**——
    /// 只 `request_redraw` 而不标脏,那一帧会在 `frame_is_dirty` 处被判 Idle 丢掉。
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
        let (Some(a), Some(ws)) = (self.active.as_ref(), self.ws.as_ref()) else {
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
        let f = self.ws.as_ref()?.focus();
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
        if self.ws.is_none() {
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
            if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
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
        let win_h = a.gpu.config.height as f32;
        let cell_h = a.text.cell_h;
        self.autoscroll = input::autoscroll_lines(self.cursor_px.1, win_h, cell_h);
        if let Some((col, row, side)) = self.selection_cursor() {
            if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
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
                if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
                    pane.emulator.selection_clear();
                }
                self.request_ui_redraw();
                return;
            }
        }
        self.copy_selection();
    }

    /// 把当前选区写进系统剪贴板。无选区 = 什么都不做(`selection_text` 返回
    /// `None`),不能写空串——那会清掉用户剪贴板里原有的内容。
    fn copy_selection(&mut self) {
        let Some(text) = self
            .ws
            .as_ref()
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
        if self.ws.is_none() {
            return;
        }
        let Some(text) = self.clipboard.get() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let bracketed = self
            .ws
            .as_ref()
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
    /// 当前的四处以 `grep -n "pty.write" crates/mullion-app/src/app.rs` 为准
    /// (行号会漂,别钉死)。
    fn user_took_over(&mut self) {
        if let Some(h) = self.automation.as_mut() {
            // drop 发送端即取消(`write_scheduled` 的 doc:收到值**或**发送端
            // 被 drop 都算取消)。
            h.cancel.take();
        }
    }

    /// 真正发送。到这里要么不需要确认,要么用户已经点了「粘贴」。粘贴目标
    /// 是**焦点 pane**——分屏后粘贴永远只进当前正在操作的那一块。
    fn send_paste(&mut self, text: &str) {
        let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) else {
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
        if let Some(ws) = self.ws.as_mut() {
            ws.pump(now);
        }
        self.drive_automation();
    }

    /// 首字节 / 断线两条边。挂在 `pump_io` 上而不是重绘上:最小化期间窗口
    /// 未必还会被重绘,但 `Wake` 仍会驱动 `pump_io`——否则用户最小化着连上,
    /// 自动化会一直等到超时。
    ///
    /// 每帧调,所以**零分配**:只读两个 bool、`take()` 两个 `Option`。
    fn drive_automation(&mut self) {
        let Some(h) = self.automation.as_mut() else {
            return;
        };
        let Some(ws) = self.ws.as_ref() else {
            return;
        };
        // pane 不在了(被关掉/换世代):让 task 自然结束,别让它挂到超时。
        let Some(pane) = ws.pane(h.pane) else {
            h.disconnect.take();
            return;
        };
        if pane.status == crate::shell::workspace::PaneStatus::Disconnected {
            // send 的 Err(接收端已走)无所谓:task 已经结束了。
            if let Some(tx) = h.disconnect.take() {
                let _ = tx.send(());
            }
            return;
        }
        if pane.saw_first_byte {
            if let Some(tx) = h.ready.take() {
                let _ = tx.send(());
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
    fn spawn_connect(&mut self, cfg: SshConfig) {
        // F40~F44:此刻才确定「是哪条会话」。连接在途期间用户可能改配置甚至
        // 删会话,所以计划必须在用户点击的这一帧定死。
        // 上一次的结论到此为止:新连接开始了,旧结论就是误导信息。
        self.automation_status = None;
        self.pending_automation =
            crate::automation::pending_for(self.ui.connect_request_last, |id| {
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
                Some((resolved.automation, name))
            });
        // 会话管理器发起的连接也要记下,否则第二次连接后开分屏会用上一台
        // 主机的 term/尺寸(F35 的 open_pty 靠它)。
        self.last_cfg = Some(cfg.clone());
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
            match mullion_ssh::session::open_pty(handle.clone(), &cfg, wake).await {
                Ok((ssh, rx)) => {
                    let _ = proxy.send_event(UserEvent::ConnectOk { ssh, rx, handle });
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
        let Some(ws) = self.ws.as_ref() else { return };
        let Some(host) = ws.hosts.first() else { return };
        // C1:开 channel 是异步的,回来时用户可能已经断开重连、换了一个新
        // `Workspace`(`next_id` 重新从 2 计数,`id` 会撞号)。把发起时刻的
        // 世代一起带走,`PaneOpened`/`PaneOpenErr` 抵达时据此判断"这事件还
        // 是不是当前这个 Workspace 发出的"。
        let generation = ws.generation();
        let handle = host.handle.clone();
        let Some(cfg) = self.last_cfg.clone() else {
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

    /// 私钥文件对话框:另起线程跑,结果经 `proxy` 回送。
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
    fn spawn_key_picker(&self) {
        let mut dialog = rfd::FileDialog::new().set_title("选择私钥文件");
        if let Some(a) = &self.active {
            dialog = dialog.set_parent(a.window.as_ref());
        }
        let proxy = self.proxy.clone();
        let spawned = std::thread::Builder::new()
            .name("mullion-file-dialog".into())
            .spawn(move || {
                let picked = dialog.pick_file();
                let _ = proxy.send_event(UserEvent::KeyPathPicked(picked));
            });
        if let Err(e) = spawned {
            // 起不了线程就退回「没选中」,让 busy 标志复位,UI 不会卡在按不动。
            log::warn!(target: "mullion", "文件对话框线程创建失败: {e}");
            let _ = self.proxy.send_event(UserEvent::KeyPathPicked(None));
        }
    }

    /// 自动化结束。**必须按世代过滤**:高延迟链路下用户完全可能在自动化还在
    /// 跑的时候断开重连,旧世代的「自动化已中止:连接已断开」落到新连接的
    /// 状态栏上,是一条与当前连接毫不相干的误导信息(判据同 `PaneOpenErr`)。
    fn accept_automation_done(&mut self, generation: u64, outcome: crate::automation::Outcome) {
        if !self
            .ws
            .as_ref()
            .is_some_and(|ws| generation_matches(ws, generation))
        {
            log::debug!(target: "mullion", "丢弃过期世代 {generation} 的自动化结论");
            return;
        }
        log::info!(target: "mullion", "自动化结束: {outcome:?}");
        self.automation_status = Some(crate::automation::status_text(outcome));
        self.automation = None;
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
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("mullion"))
                .expect("create_window"),
        );
        let init_size = window.inner_size();
        crate::logx::line(&format!(
            "resumed: 窗口创建 {}x{} scale={}",
            init_size.width,
            init_size.height,
            window.scale_factor()
        ));
        let gpu = Gpu::new(window.clone(), self._runtime.handle());
        // 字号 10pt,按窗口 DPI 缩放成物理像素(inner_size 是物理像素,须一致):
        // px = pt * (96*scale/72)。Windows 常见 125%/150% 缩放下才不会过小。
        // TODO:字体/字号做成可配置 + 跟随 ScaleFactorChanged 动态更新(见 spec F21)。
        let scale = window.scale_factor() as f32;
        let font_px = FONT_POINT_SIZE * scale * 96.0 / 72.0;
        let text = TextLayer::new(
            &gpu.device,
            &gpu.queue,
            gpu.config.format,
            font_px,
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
        let dir = crate::shell::store::config_dir();
        self.store = match dir {
            Some(d) => match crate::shell::store::SessionStore::open(
                d,
                &mullion_store::KeyringSource::new(),
            ) {
                Ok(s) => {
                    crate::logx::line(&format!("会话库已打开,{} 个会话", s.list().len()));
                    Some(s)
                }
                Err(e) => {
                    crate::logx::line(&format!("会话库打开失败: {e}"));
                    self.ui.set_error(format!("会话库打开失败:{e}"));
                    None
                }
            },
            None => {
                crate::logx::line("无法定位配置目录,会话功能禁用");
                self.ui.set_error("无法定位配置目录".into());
                None
            }
        };
        // 启动时先算一次,否则第一次打开会话管理器全是无色。
        self.refresh_appearance();

        // CLI 直连(路径①)→ 立刻发起连接,进终端态;无参启动(路径②)→ 留在
        // launcher(conn 仍 None)并自动弹出会话管理器,让用户选/建会话(§2/Task7)。
        if let Some(cfg) = self.initial.take() {
            self.spawn_connect(cfg);
        } else {
            self.ui.session_manager_open = true;
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
            UserEvent::ConnectOk { ssh, rx, handle } => {
                crate::logx::line("连接成功,进入终端态");
                // 一旦连上就进入交互态:后续(哪怕是本次会话断开后)的连接失败
                // 不再是「CLI 直连首次失败」,不该导致整个 GUI exit(1)(复核 #1)。
                self.cli_direct = false;
                let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
                let d = theme::term_default_colors(&MULLION_DARK);
                emulator.set_default_colors(d.fg, d.bg);
                // C1:每次连接都是全新世代——`next_ws_generation` 取值后自增,
                // 保证跟上一次(如果有)断开的那个 Workspace 的世代号不同,
                // 哪怕 PaneId 因为 next_id 重新计数而撞号,也能靠这个分辨。
                let generation = self.next_ws_generation;
                self.next_ws_generation += 1;
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
                    },
                    generation,
                );
                ws.hosts.push(crate::shell::workspace::HostConn {
                    label: self
                        .last_cfg
                        .as_ref()
                        .map_or_else(|| "远端".to_string(), |c| format!("{}@{}", c.user, c.host)),
                    addr: self
                        .last_cfg
                        .as_ref()
                        .map_or_else(String::new, |c| format!("{}:{}", c.host, c.port)),
                    // 与紧邻的 `self.connected_session` 同源:都取发起这次连接时
                    // 记下的那条会话(`ConnectOk` 事件本身不带 SessionId)。
                    session_id: self.ui.connect_request_last,
                    handle,
                });
                self.ws = Some(ws);
                self.current_preset = Some(Preset::Single);
                // 连上后关掉会话管理弹窗,别让它盖在新终端上方(复核 #4)。
                self.ui.close_session_manager();
                // ConnectOk 不带 SessionId(见 UserEvent 定义),用发起连接时
                // 记下的那条。状态点只区分「这条连上了 / 没连上」两态。
                self.connected_session = self.ui.connect_request_last;
                self.ui_dirty = true;
                // F40~F44:起自动化。旧那次(如果有)的结论对新连接没有意义。
                if let Some(old) = self.automation.take() {
                    old.task.abort();
                }
                if let Some(plan) = crate::automation::take_pending(
                    &mut self.pending_automation,
                    &mut self.pending_skip_automation,
                ) {
                    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
                    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();
                    let (disc_tx, disc_rx) = tokio::sync::oneshot::channel();
                    let sink: Arc<dyn mullion_ssh::schedule::ByteSink> = ssh;
                    let proxy = self.proxy.clone();
                    let steps = plan.steps.len();
                    let timeout_ms = plan.ready_timeout_ms;
                    log::info!(
                        target: "mullion",
                        "自动化:{steps} 步待发,就绪超时 {timeout_ms}ms"
                    );
                    let task = self._runtime.spawn(async move {
                        let outcome = crate::automation::run(
                            sink, plan.steps, ready_rx, cancel_rx, disc_rx, timeout_ms,
                        )
                        .await;
                        let _ = proxy.send_event(UserEvent::AutomationDone(generation, outcome));
                    });
                    self.automation = Some(AutomationHandle {
                        // 只有第一个 pane 跑自动化(总设计 §7 前提②)。
                        pane: PaneId(1),
                        ready: Some(ready_tx),
                        cancel: Some(cancel_tx),
                        disconnect: Some(disc_tx),
                        task,
                    });
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
                if let Some(ws) = self.ws.as_mut() {
                    // 开 channel 是真实网络往返(高延迟代理链路下可能要几百 ms 到
                    // 几秒),这期间用户完全可能又切了预设,甚至断开重连出了一个
                    // 全新的 Workspace(C1:`next_id` 重新计数,`id` 会跟旧世代
                    // 撞号)——不查树成员 + 世代直接 attach_pane 的后果:轻则是
                    // 孤儿 pane(不出现在 compute_geoms/渲染/标题条里,`pump`
                    // 却仍在每帧驱动它,SSH channel 永远占着不关),重则是顶掉
                    // 新世代刚建好、正常工作的 PaneState(输入从此写进一条已经
                    // 不存在意义的旧连接)。
                    if pane_still_wanted(ws, id, generation) {
                        let mut emulator = mullion_term::emulator::Emulator::new(80, 24);
                        let d = theme::term_default_colors(&MULLION_DARK);
                        emulator.set_default_colors(d.fg, d.bg);
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
                if self
                    .ws
                    .as_ref()
                    .is_some_and(|ws| generation_matches(ws, generation))
                {
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
                if self.cli_direct && self.ws.is_none() {
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
            UserEvent::AutomationDone(generation, outcome) => {
                self.accept_automation_done(generation, outcome);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        diag::mark(diag::Stage::WindowEvent);
        // 输入分流(§4.5)。**键盘与指针的顺序是反的,不是笔误**:
        // - 指针:先喂 egui 再判。egui 要靠 `CursorMoved` 维护 hover,不喂就没有
        //   `wants_pointer_input()` 可言。
        // - 键盘:先判再决定喂不喂(T8)。喂给 egui 的键会先经它的焦点系统——Tab 会被
        //   拿去把焦点给菜单栏第一个按钮,此后 `wants_keyboard_input()` 恒 true,
        //   下面的 route 把每个按键都判给 egui,终端永久收不到键。
        if let Some(active) = &mut self.active {
            let is_kbd = matches!(event, WindowEvent::KeyboardInput { .. });
            let is_ptr = matches!(
                event,
                WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::CursorMoved { .. }
            );
            let modal = self.ui.session_manager_open
                || self.ui.about_open
                || self.pending_host_key.is_some()
                || self.pending_paste.is_some();
            // 键盘归终端时整段跳过 egui;其余事件(含指针与 resize/focus 等)照旧喂。
            if is_kbd
                && !shell::input_route::egui_should_see(
                    shell::input_route::InputKind::Keyboard,
                    modal,
                    active.egui_ctx.wants_keyboard_input(),
                )
            {
                // Route::Terminal → 直落下面 UNCHANGED 的 KeyboardInput 分支(守 T5/T6)。
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
        match event {
            WindowEvent::CloseRequested => {
                crate::logx::line("CloseRequested → 退出");
                // F92:进程要走了,20 秒的 timeout 别悬着。
                if let Some(h) = self.probe_task.take() {
                    h.abort();
                }
                event_loop.exit();
            }
            // 焦点/遮挡:记录以便定位「失焦后无法回到前台/黑屏」;回到前台时补一次
            // 重绘,避免停在陈旧/空白帧(此前这些事件落 `_ => {}`,不重绘也不留痕)。
            WindowEvent::Focused(focused) => {
                crate::logx::line(&format!("Focused({focused})"));
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
                // DPI 跟随是 F21,这里暂不重建字体,仅记录(跨屏 DPI 变化时字号不更新)。
                crate::logx::line(&format!("ScaleFactorChanged({scale_factor})"));
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
                // 先算,下面 `self.ws.as_mut()` 一借出去就没法再调 `&self` 方法了。
                let local = self.cursor_in_grid();
                let geom = self.focused_geom();
                if let (Some(a), Some(g), Some(pane)) = (
                    self.active.as_ref(),
                    geom,
                    self.ws.as_mut().and_then(Workspace::focused_mut),
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
                        if let Some(ws) = self.ws.as_mut() {
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
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
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
                            if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
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
                        if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
                            // F18:一按普通键就清选区。留着的话高亮会挂在屏幕上,
                            // 而底下的内容早被新输出冲掉了——高亮的是别的字。
                            pane.emulator.selection_clear();
                            // F17:一按普通键就贴回底部,否则「打字了但看不到自己输入」。
                            pane.emulator.scroll_to_bottom();
                            let _ = pane.pty.write(bytes);
                            // F40:用户接管,自动化让位(借用已释放)。
                            self.user_took_over();
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
                let dirty = match &self.ws {
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
                                .ws
                                .as_ref()
                                .map(|ws| {
                                    geoms
                                        .iter()
                                        .filter_map(|g| {
                                            ws.pane(g.id).map(|p| (*g, p.emulator.snapshot()))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            let focus = self.ws.as_ref().map(Workspace::focus);
                            let renders: Vec<crate::gpu::PaneRender<'_>> = snaps
                                .iter()
                                .map(|(g, s)| crate::gpu::PaneRender {
                                    geom: *g,
                                    snap: s,
                                    focused: Some(g.id) == focus,
                                })
                                .collect();
                            let titles: Vec<crate::ui::pane_title::TitleView<'_>> = self
                                .ws
                                .as_ref()
                                .map(|ws| {
                                    geoms
                                        .iter()
                                        .enumerate()
                                        .map(|(i, g)| crate::ui::pane_title::TitleView {
                                            geom: *g,
                                            index: i + 1,
                                            host: ws.pane(g.id).and_then(|p| {
                                                ws.hosts.get(p.host_ix).map(|h| h.label.as_str())
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
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            let groups: &[mullion_store::GroupRecord] =
                                self.store.as_ref().map_or(&[], |s| s.groups());
                            let frame = crate::ui::UiFrame {
                                sessions,
                                groups,
                                store_available,
                                connected: self.ws.is_some(),
                                panes: self.ws.as_ref().map_or(1, Workspace::pane_count),
                                preset: self.current_preset,
                                titles: &titles,
                                host_key: host_key_view,
                                paste: paste_view,
                                secret_presence: match (self.store.as_ref(), self.ui.editor_id) {
                                    (Some(s), Some(id)) => s.secret_presence(id),
                                    _ => crate::ui::session_manager::SecretPresence::default(),
                                },
                                connected_session: self.connected_session,
                                // 「跑着的时候盖住上一次的结论」这条规则放在
                                // `automation::status_line` 里,不在这儿手写
                                // if/else —— 写反的现象是新连接的状态栏挂着上
                                // 一条连接的结论,而它有单测钉着。
                                automation: crate::automation::status_line(
                                    self.automation.is_some(),
                                    self.automation_status.as_deref(),
                                ),
                                appearance: &self.appearance,
                            };
                            let a = self.active.as_mut().expect("上面刚判过 is_some");
                            let (repaint_delay, actions) =
                                render_frame(a, &renders, &mut self.ui, frame);
                            drop(renders);
                            drop(titles);
                            drop(snaps);

                            self.limiter.record_present(now);
                            // egui 侧已画出;下面若 egui 又要一帧会重新置脏。
                            self.ui_dirty = false;
                            // 施加几何:F34/T4 的唯一出口。本帧 build_ui 刚写入的
                            // central_px 要下一帧才生效(与 B0 起就是这个语义)。
                            if let Some(ws) = self.ws.as_mut() {
                                for p in ws.panes_mut_iter() {
                                    p.pacer.mark_presented();
                                }
                                ws.apply_geometry(&geoms);
                            }
                            // 布局动作:点了预设 / 点了标题条的 ×。路由逻辑在自由函数
                            // `apply_layout_actions`(只碰 &mut Workspace,可脱离
                            // runtime/proxy 单测);真正开新 channel 需要 runtime/proxy,
                            // 落在 `spawn_fresh_panes`。
                            if let Some(ws) = self.ws.as_mut() {
                                if let Some((fresh, preset_out)) =
                                    apply_layout_actions(ws, &actions)
                                {
                                    self.current_preset = preset_out;
                                    self.ui_dirty = true;
                                    self.spawn_fresh_panes(fresh);
                                }
                            }
                            // F83 标题条开关:改的是行数,下一帧 compute_geoms
                            // 算出新 grid,再由 apply_geometry 发 window_change。
                            if self.ui.toggle_title_bars {
                                self.ui.toggle_title_bars = false;
                                if let Some(ws) = self.ws.as_mut() {
                                    ws.title_bars = !ws.title_bars;
                                }
                                self.ui_dirty = true;
                            }
                            // 菜单动作(§4.2):断开回到 launcher 态 / 退出整个事件循环。
                            if self.ui.request_disconnect {
                                self.ui.request_disconnect = false;
                                self.ws = None;
                                // 与 self.ws 成对维护:断开后不清会一直显示
                                // 「已连接」的陈旧状态点。
                                self.connected_session = None;
                                // F40:自动化 task 也持有一份 `Arc<SshSession>`。
                                // 不 abort 的话,`self.ws = None` 只 drop 掉 pane 那
                                // 一份,`io_task` 因 cmd_tx 仍有克隆而不会收口 ——
                                // 用户点了「断开」、UI 回到 launcher 态,预配置的命令
                                // 却还在往一条没真正断开的 channel 上发。
                                // `drive_automation` 补不了这条边:它在 ws 为 None 时
                                // 直接 return。
                                if let Some(h) = self.automation.take() {
                                    h.task.abort();
                                }
                            }
                            if self.ui.request_quit {
                                self.ui.request_quit = false;
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
                            } else if let Some(at) = self.sync_timeout_wake(now) {
                                // Important #2/T2:egui 这帧不需要重绘,但有 pane 卡在
                                // 未超时的同步块里——主动排一次到超时点的唤醒,而不是
                                // 无条件 Wait 等下一个不相关事件顺带救回冻住的画面。
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
                        // Important #2/T2:同上——没有脏帧不代表没有 pane 卡在同步块里。
                        if let Some(at) = self.sync_timeout_wake(now) {
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
                    if let Some(pane) = self.ws.as_mut().and_then(Workspace::focused_mut) {
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

                // Task 6:会话管理弹窗的 intent 施加点。放在 `plan` 整块之后——此处
                // self.active/self.ws/self.ui 的借用都已释放,才能拿 `&mut
                // self.store`(egui 闭包里借不到它,只能在这里事后统一施加)。
                // `touched_store` 必须在三个 `take()` 之前算:`take()` 之后就
                // 问不出「刚才有没有意图」了。F61/F62 的外观缓存要在会话/分组
                // 变更后重算。
                let touched_store = self.ui.delete_request.is_some()
                    || self.ui.save_request.is_some()
                    || self.ui.group_intent.is_some();
                if self.ui.delete_request.is_some() || self.ui.save_request.is_some() {
                    // keyring/TOML 是同步 IO,在事件回调里可能阻塞(Windows 凭据管理器
                    // 偶发几百 ms),打点让看门狗能指认。
                    diag::mark(diag::Stage::StoreIo);
                }
                if let Some(id) = self.ui.delete_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        if let Err(e) = store.delete(id).and_then(|_| store.save()) {
                            self.ui.set_error(format!("删除失败:{e}"));
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
                                if then_connect {
                                    self.ui.connect_request = Some(id);
                                }
                            }
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
                    self.spawn_key_picker();
                }
                // 连接:双击行 / 点「连接」。必须在 store 的 &mut 借用结束后调
                // (下面 `self.store.as_ref()` 的临时借用在 match 表达式求值完就
                // 释放,故可紧接着调 self.spawn_connect)。
                // F44:**无条件**取走跳过标志 —— 哪怕这一帧没有连接意图(右键
                // 点了又关掉菜单),也不能让它漂到下一次连接上。
                let skip_automation = std::mem::take(&mut self.ui.connect_skip_automation);
                if let Some(id) = self.ui.connect_request.take() {
                    self.ui.connect_request_last = Some(id);
                    match self.store.as_ref().map(|s| s.ssh_config_for(id)) {
                        Some(Ok(cfg)) => {
                            // 用户主动发起的连接是交互态,不该继承 CLI 直连的
                            // exit(1) 语义(复核 #1)。
                            self.cli_direct = false;
                            // 跳过标志必须跟 `pending_automation` 同进同退 ——
                            // 后者只在 `spawn_connect` 里写。写在 match 外面的话,
                            // 一次**失败**的连接尝试(配置坏了走 Err 支)会把标志
                            // 留给另一条还在途的连接:用户没点过跳过,那条连接的
                            // 自动化却被 `take_pending` 静默丢掉。
                            self.pending_skip_automation = skip_automation;
                            self.spawn_connect(cfg);
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

/// C1:事件携带的世代号是否与当前 `Workspace` 一致——`PaneOpened` 与
/// `PaneOpenErr` 共用同一条判断,避免两处各写一遍、将来改一处漏改另一处。
fn generation_matches(ws: &Workspace, generation: u64) -> bool {
    ws.generation() == generation
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
/// 纯函数(只读 `&Workspace`),不碰 `EventLoopProxy`,可脱离真实事件循环单测。
fn pane_still_wanted(ws: &Workspace, id: PaneId, generation: u64) -> bool {
    generation_matches(ws, generation) && mullion_core::layout::leaves(ws.tree()).contains(&id)
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
    let deadline_ms =
        crate::render::earliest_sync_timeout_ms(ws.panes().iter().map(|p| &p.pacer), now_ms)?;
    Some(start + std::time::Duration::from_millis(deadline_ms))
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

/// 一帧渲染:先跑 egui(菜单栏 + 工具栏 + 状态栏 + 标题条,§4.2),再(终端态时)
/// 叠加背景色块 + 文字前景趟。返回 (egui 想要的下次重绘时间, 本帧的布局动作)——
/// 前者 `Duration::MAX` = 不需要,调用方据此走 T3/T7 的 `next_frame_at`/
/// `WaitUntil`,不会无条件 `request_redraw`;后者由调用方在借用释放后统一施加。
/// GPU 胶水,无单测。
fn render_frame(
    a: &mut Active,
    panes: &[crate::gpu::PaneRender<'_>],
    ui_state: &mut crate::ui::UiState,
    frame: crate::ui::UiFrame<'_>,
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
    let full_output = a.egui_ctx.run(raw_input, |ctx| {
        let this_pass = crate::ui::build_ui(ctx, &MULLION_DARK, ui_state, frame);
        // `UiActions` 没有 derive `PartialEq`,这里是逐字段手写的"是否有真实动作"——
        // 给 `UiActions` 加新字段时必须在这里补上对应的 `.is_some()`(或等价判断),
        // 否则新动作会在上面文档注释说的 discard 趟里被静默丢弃。
        if this_pass.preset.is_some() || this_pass.close_pane.is_some() {
            actions = this_pass;
        }
    });
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
        apply_layout_actions, apply_save, generation_matches, pane_still_wanted,
        sync_timeout_wake_at,
    };
    use crate::frame::FrameLimiter;
    use crate::reflow::{reflow, ResizeSink};
    use crate::shell::workspace::{Preset, Workspace};
    use mullion_core::layout::{Dir, Node, PaneId, Rect};

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
        let geoms = layout_geometry(&tree, area, (10.0, 20.0), true);
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

    /// 复核 C1:`PaneOpenErr` 直接调 `generation_matches`(不经过
    /// `pane_still_wanted`,因为它不关心 id/树成员,只关心"这条失败提示还是
    /// 不是当前世代的")——单独锁住这条判断,不依赖 `pane_still_wanted` 的
    /// 组合行为碰巧带出覆盖。旧世代的失败提示如果不过滤,会给用户弹一条跟
    /// 当前连接毫不相干的错误 toast。
    #[test]
    fn generation_matches_only_accepts_the_current_workspaces_generation() {
        let ws = Workspace::new(test_pane(1), 3);
        assert!(
            generation_matches(&ws, 3),
            "世代号一致,该事件属于当前 Workspace"
        );
        assert!(
            !generation_matches(&ws, 2),
            "世代号不一致(旧世代迟到的事件),不该被当成当前世代的"
        );
    }

    /// **接线守护**:`accept_automation_done` 必须真的调 `generation_matches`。
    ///
    /// 上面那条测试只锁住了 `generation_matches` 这个 helper 本身;把
    /// `accept_automation_done` 里的过滤整段删掉,全量测试依然全绿 —— 说明
    /// 「这个函数有没有用上那个判据」是无人守护的。而这正是它存在的全部理由:
    /// 高延迟链路下用户完全可能在自动化还在跑时断开重连,旧世代的
    /// 「自动化已中止:连接已断开」落到新连接的状态栏上,是一条与当前连接
    /// 毫不相干的误导信息。
    ///
    /// **扎的是源码结构而非运行时行为**,这是刻意的:`App` 要
    /// `EventLoopProxy` 才能构造,单测里造不出来。验证边界:它只挡得住
    /// 「函数体里没有 generation_matches」这一种写法,挡不住有人换个同义
    /// 判据或把过滤写成永真。
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
            body.contains("generation_matches"),
            "accept_automation_done 里没有世代过滤 —— 旧连接迟到的自动化结论\
             会覆盖新连接的状态栏,给用户看一条与当前连接毫不相干的信息"
        );
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

    /// **接线守护**:用户意图写入点的数量。
    ///
    /// `user_took_over` 的文档说「当前的四处以 grep 为准」——这条测试把那句话
    /// 变成可执行断言。四个写入点里滚轮的两处(`Report`/`ArrowKeys`)共用分支
    /// 末尾一次调用,所以调用点是三处。
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
            calls, 3,
            "用户意图写入点的取消调用应为 3 处(粘贴/滚轮/键盘,滚轮两个分支\
             共用一次),实际 {calls} 处 —— 少了会让自动化在用户打字时继续发\
             命令,多了说明新增了输入路径但没复核这条不变量"
        );
    }

    /// **接线守护**:「断开连接」必须把自动化 task abort 掉。
    ///
    /// 自动化 task 也持有一份 `Arc<SshSession>`。只把 `self.ws` 置 `None`
    /// 的话,`SshSession` 的 `cmd_tx` 仍有活着的克隆,`io_task` 不会收口 ——
    /// 用户点了「断开」、UI 回到 launcher 态,预配置的命令却还在往一条没真正
    /// 断开的 channel 上发,用户既看不到也拦不住。`drive_automation` 补不了
    /// 这条边:它在 `self.ws` 为 `None` 时直接 return。
    ///
    /// **扎的是源码结构而非运行时行为**:`App` 要 `EventLoopProxy` 才能构造,
    /// 单测里造不出来。验证边界:只挡得住「断开分支里没有 abort」这一种写法,
    /// 挡不住有人把 abort 写在够不到的分支里。
    ///
    /// 自证会变红:把断开分支里的 `h.task.abort()` 那三行删掉。
    #[test]
    fn disconnect_aborts_the_automation_task() {
        let src = include_str!("app.rs");
        let after = src
            .split("if self.ui.request_disconnect {")
            .nth(1)
            .expect("找不到断开连接的分支");
        let body = &after[..after
            .find("\n                            }\n")
            .expect("找不到断开连接分支的结尾")];
        assert!(
            body.contains("self.automation.take()") && body.contains("abort()"),
            "断开连接时没有 abort 自动化 task —— 它持有的那份 Arc<SshSession> \
             会让底层 io_task 不收口,断开后预配置的命令还在继续发"
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
        match &rec.auth.kind {
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
}
