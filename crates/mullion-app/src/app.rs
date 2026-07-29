//! App:winit ApplicationHandler<UserEvent>。持有窗口/GPU/文字层/运行时,以及一个
//! `Option<Connection>`(launcher 态 None / 终端态 Some,§2.2)。每帧(conn 存在时)
//! 「排空 rx → feed emu → 回写 PtyWrite(T1)」,GPU present 受帧率(T3)与同步块(T2)双闸。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use mullion_core::layout::PaneId;
use mullion_ssh::config::SshConfig;
use mullion_ssh::known_hosts::HostKeyPolicy;
use mullion_ssh::session::SshSession;
use mullion_store::known_hosts::{HostKeyEntry, KnownHostsFile};
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
use crate::gpu::{quads_for, Gpu};
use crate::pane::Pane;
use crate::render::SyncFramePacer;
use crate::text::TextLayer;
use crate::theme::{self, MULLION_DARK};
use crate::{diag, grid, input, session_pump, shell};

/// app 与「连接建立」异步任务之间的事件(ssh io_task / connect 的 wake、结果经此回送)。
/// 携带 `SshSession`/`Receiver` 等非 `Copy` 负载,故不能派生 Copy/Clone;两者也未实现
/// `Debug`,故 `UserEvent` 同样不派生 Debug(winit `ApplicationHandler<T>` 只要求 `T: 'static`)。
pub enum UserEvent {
    Wake,
    /// 异步 connect 成功:句柄 + 远端字节接收端(app 每帧 drain)。
    ConnectOk {
        ssh: SshSession,
        rx: Receiver<Vec<u8>>,
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
}

/// 窗口出现后才建的 GPU 相关状态。
struct Active {
    window: Arc<Window>,
    gpu: Gpu,
    text: TextLayer,
    grid_dims: (u16, u16),
    egui_ctx: egui::Context,
    egui_state: egui_winit::State,
    egui_renderer: egui_wgpu::Renderer,
}

/// 一条活跃连接的全部状态。launcher 态时 `App::conn` 是 `None`。
struct Connection {
    ssh: SshSession,
    rx: Receiver<Vec<u8>>,
    pane: Pane,
    pacer: SyncFramePacer,
}

pub struct App {
    _runtime: Runtime,
    /// `None` = launcher 态(无终端字节可处理);`Some` = 终端态。
    conn: Option<Connection>,
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
            conn: None,
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
            initial,
            cli_direct,
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
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    /// UI 侧变了(或 egui 自己要重绘):标脏 + 请求一帧。**两件事必须一起做**——
    /// 只 `request_redraw` 而不标脏,那一帧会在 `frame_is_dirty` 处被判 Idle 丢掉。
    fn request_ui_redraw(&mut self) {
        self.ui_dirty = true;
        if let Some(a) = &self.active {
            a.window.request_redraw();
        }
    }

    /// 指针位置换算成**终端区局部**像素坐标(窗口坐标减去 egui 中央区原点)。
    ///
    /// 终端自绘层整体平移到了菜单栏之下(`ui::UiState::central_origin_px`),
    /// 所有「像素 → 单元格」的换算都必须用同一个原点,否则鼠标点到的格子与
    /// 眼睛看到的差一个菜单栏的高度。平移**只在这一个函数里做**。
    /// 负数(指针在菜单栏/状态栏上)交给 `cell_at`/`cell_side` 各自夹紧。
    fn cursor_in_grid(&self) -> (f32, f32) {
        let (ox, oy) = self.ui.central_origin_px;
        (self.cursor_px.0 - ox, self.cursor_px.1 - oy)
    }

    /// 指针当前位置对应的 **0-based** viewport 单元格与格内左右半。
    ///
    /// `input::cell_at` 给的是 **1-based**(F17 鼠标上报的口径,SGR 协议要求),
    /// 而选区 API 收 0-based。两套口径并存是既有事实,换算**只在这一个函数里做**,
    /// 别让 0/1 混进事件循环——那是 off-by-one 最容易长出来的地方。
    fn selection_cursor(&self) -> Option<(u16, u16, mullion_term::selection::CellSide)> {
        let a = self.active.as_ref()?;
        let cell_px = (a.text.cell_w, a.text.cell_h);
        let local = self.cursor_in_grid();
        let (col1, row1) = input::cell_at(local, cell_px, a.grid_dims);
        let side = input::cell_side(local.0, cell_px.0, a.grid_dims.0);
        Some((col1.saturating_sub(1), row1.saturating_sub(1), side))
    }

    /// 左键按下:判连击类型 → 开新选区(旧选区被覆盖)。
    fn selection_press(&mut self) {
        // 没有连接就没有终端可选,别让 `dragging` 在 launcher 态被置起来——
        // 那会让后续每次 `CursorMoved` 都白跑一遍划选和重绘。
        if self.conn.is_none() {
            return;
        }
        let Some(a) = self.active.as_ref() else {
            return;
        };
        let cell_px = (a.text.cell_w, a.text.cell_h);
        let pos1 = input::cell_at(self.cursor_in_grid(), cell_px, a.grid_dims);
        let (kind, prev) = input::click_kind(self.prev_click, Instant::now(), pos1);
        self.prev_click = Some(prev);
        if let Some((col, row, side)) = self.selection_cursor() {
            self.press_anchor = Some(((col, row), kind));
            if let Some(conn) = self.conn.as_mut() {
                conn.pane.emulator.selection_start(col, row, kind, side);
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
            if let Some(conn) = self.conn.as_mut() {
                conn.pane.emulator.selection_update(col, row, side);
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
                if let Some(conn) = self.conn.as_mut() {
                    conn.pane.emulator.selection_clear();
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
            .conn
            .as_ref()
            .and_then(|c| c.pane.emulator.selection_text())
        else {
            return;
        };
        self.clipboard.set(&text);
    }

    /// 右键 / `Ctrl+Shift+V`:读剪贴板 → 判断要不要先确认 → 发送。
    fn request_paste(&mut self) {
        // 没有连接就没有地方可贴。不早退的话,launcher 态右键会读剪贴板、
        // 多行内容还会弹出一个「确认粘贴」窗——点了「粘贴」却什么都不会发生
        // (`send_paste` 拿不到 conn 直接返回)。与 `selection_press` 同一道门。
        if self.conn.is_none() {
            return;
        }
        let Some(text) = self.clipboard.get() else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let bracketed = self.conn.as_ref().is_some_and(|c| {
            c.pane
                .emulator
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

    /// 真正发送。到这里要么不需要确认,要么用户已经点了「粘贴」。
    fn send_paste(&mut self, text: &str) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let bracketed = conn
            .pane
            .emulator
            .mode()
            .contains(mullion_term::TermMode::BRACKETED_PASTE);
        let bytes = mullion_term::keymap::encode_paste(text, bracketed);
        // 与按键同理(F17):贴之前先回底部,否则「贴了但看不到」。
        conn.pane.emulator.scroll_to_bottom();
        let _ = conn.ssh.write(bytes);
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
        // 走与还原同一条路径:最小化期间跳过的 surface configure 与 grid 传播都要补上。
        self.apply_resize(size.width, size.height);
    }

    /// 应用一次窗口尺寸变化(`Resized` 事件与 Minimized 自愈共用)。
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
        // 最小化(0×0)时三件事全不做:configure 0 面积表面、按 1 列 reflow 碾平
        // scrollback、把 window_change 1×1 发给远端。
        if plan.reconfigure_surface {
            a.gpu.resize(width, height);
        }
        if plan.propagate_grid {
            let (cols, rows) = grid::grid_size_for(width, height, a.text.cell_w, a.text.cell_h);
            if (cols, rows) != a.grid_dims {
                a.grid_dims = (cols, rows);
                // 单 pane MVP 直接 resize;多 pane 的 reflow(ResizeSink)留给 F4 分屏。
                // launcher 态(conn=None)没有终端可 resize,跳过。
                if let Some(conn) = self.conn.as_mut() {
                    conn.pane.emulator.resize(cols, rows);
                    let _ = conn.ssh.resize(cols, rows); // T4
                }
            }
        }
        if plan.request_redraw {
            self.request_ui_redraw();
        }
    }

    /// 排空 rx → feed emulator → 回写 `PtyWrite`(T1 红线)。
    ///
    /// 从 `RedrawRequested` 和(最小化时)`UserEvent::Wake` 两处调:最小化期间窗口
    /// 未必还会被重绘,不能把这条通路挂在重绘上,否则有界 rx(256)灌满堵住 io_task,
    /// 远端的同步输出探测/光标查询永久等不到应答。
    fn pump_io(&mut self) {
        // 须在 conn 借用之前取:now_ms() 借整个 &self,与 conn.as_mut() 冲突。
        let now = self.now_ms();
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        diag::mark(diag::Stage::Pump);
        let mut inbound = Vec::new();
        while let Ok(bytes) = conn.rx.try_recv() {
            diag::count_inbound(bytes.len());
            inbound.push(bytes);
        }
        for b in &inbound {
            conn.pacer.feed(b, now); // T2:探测同步块
        }
        let out = session_pump::pump(&mut conn.pane.emulator, &inbound);
        if !out.is_empty() {
            let _ = conn.ssh.write(out);
        }
    }

    /// 在 `_runtime` 上异步连接;结果经 `proxy` 以 `UserEvent` 回送(§5)。
    /// 不阻塞调用方(winit 事件循环线程)。
    fn spawn_connect(&self, cfg: SshConfig) {
        let proxy = self.proxy.clone();
        let wake_proxy = self.proxy.clone();
        // 每次连接现建一个策略:它只持有两个 Arc/Sender 的克隆,构造成本可忽略,
        // 换来 App 不必长期持有一个 dyn 对象。
        let policy: Arc<dyn HostKeyPolicy> = Arc::new(crate::host_key::PromptingPolicy::new(
            self.known_hosts.clone(),
            self.proxy.clone(),
        ));
        self._runtime.spawn(async move {
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let _ = wake_proxy.send_event(UserEvent::Wake);
            });
            match mullion_ssh::session::connect(&cfg, policy, wake).await {
                Ok((ssh, rx)) => {
                    let _ = proxy.send_event(UserEvent::ConnectOk { ssh, rx });
                }
                Err(e) => {
                    let _ = proxy.send_event(UserEvent::ConnectErr(e.to_string()));
                }
            }
        });
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
        let size = window.inner_size();
        let (cols, rows) = grid::grid_size_for(size.width, size.height, text.cell_w, text.cell_h);
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
            grid_dims: (cols, rows),
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
                    self.ui.last_error = Some(format!("会话库打开失败:{e}"));
                    None
                }
            },
            None => {
                crate::logx::line("无法定位配置目录,会话功能禁用");
                self.ui.last_error = Some("无法定位配置目录".into());
                None
            }
        };

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
            UserEvent::ConnectOk { ssh, rx } => {
                crate::logx::line("连接成功,进入终端态");
                // 一旦连上就进入交互态:后续(哪怕是本次会话断开后)的连接失败
                // 不再是「CLI 直连首次失败」,不该导致整个 GUI exit(1)(复核 #1)。
                self.cli_direct = false;
                let (cols, rows) = self.active.as_ref().map_or((80, 24), |a| a.grid_dims);
                let pane = Pane::new(
                    PaneId(1),
                    cols,
                    rows,
                    theme::term_default_colors(&MULLION_DARK),
                );
                let _ = ssh.resize(cols, rows); // 初始 window_change 校正到真实尺寸(T4)
                self.conn = Some(Connection {
                    ssh,
                    rx,
                    pane,
                    pacer: SyncFramePacer::new(),
                });
                // 连上后关掉会话管理弹窗/编辑表单,别让它盖在新终端上方(复核 #4)。
                self.ui.session_manager_open = false;
                self.ui.editor_open = false;
                self.request_ui_redraw();
            }
            UserEvent::KeyPathPicked(picked) => {
                self.key_picker_busy = false;
                if let Some(p) = picked {
                    self.ui.editor.key_path = p.display().to_string();
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
                if self.cli_direct && self.conn.is_none() {
                    std::process::exit(1);
                }
                self.ui.last_error = Some(msg);
                self.request_ui_redraw();
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
                || self.ui.editor_open
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
                // 先算,下面 `self.conn.as_mut()` 一借出去就没法再调 `&self` 方法了。
                let local = self.cursor_in_grid();
                if let (Some(a), Some(conn)) = (self.active.as_ref(), self.conn.as_mut()) {
                    let cell_px = (a.text.cell_w, a.text.cell_h);
                    let lines = input::wheel_lines(delta, cell_px.1);
                    let cell = input::cell_at(local, cell_px, a.grid_dims);
                    let action = mullion_term::keymap::wheel_action(
                        conn.pane.emulator.mode(),
                        self.mods.shift_key(),
                        lines,
                        cell,
                    );
                    match action {
                        WheelAction::LocalScroll { lines } => {
                            conn.pane.emulator.scroll(Scroll::Delta(lines));
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
                            let _ = conn.ssh.write(bytes);
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
                            let _ = conn.ssh.write(bytes);
                        }
                        WheelAction::None => {}
                    }
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
                (MouseButton::Left, ElementState::Pressed) => self.selection_press(),
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
                            if let Some(conn) = self.conn.as_mut() {
                                conn.pane.emulator.scroll(scroll);
                            }
                            self.request_ui_redraw();
                            return;
                        }
                        let bytes = mullion_term::keymap::encode_key(key, mods, self.kitty);
                        // `let _` 全文件都这样:写/resize 失败(断线等)没有用户提示、
                        // 无重连。断线感知与重连是 S3,后续 spec,这里不做。
                        // launcher 态(conn=None)没有终端可写,按键静默丢弃。
                        if let Some(conn) = self.conn.as_mut() {
                            // F18:一按普通键就清选区。留着的话高亮会挂在屏幕上,
                            // 而底下的内容早被新输出冲掉了——高亮的是别的字。
                            conn.pane.emulator.selection_clear();
                            // F17:一按普通键就贴回底部,否则「打字了但看不到自己输入」。
                            conn.pane.emulator.scroll_to_bottom();
                            let _ = conn.ssh.write(bytes);
                        }
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // 计算须在 conn 借用之前:conn.as_mut() 只借 self.conn 这一字段,
                // 但 self.now_ms() 需要整个 &self,与仍存活的 conn 借用冲突。
                let now = self.now_ms();
                // 1+2. 排空 rx→feed emu→回写 PtyWrite(T1 红线)——仅终端态有字节可
                // 处理;launcher 态(conn=None)没有终端,跳过,但下面的帧率闸 + egui
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
                let dirty = match &self.conn {
                    Some(conn) => {
                        crate::frame::frame_is_dirty(conn.pacer.should_present(now), self.ui_dirty)
                    }
                    None => true,
                };
                let action = self.limiter.plan(dirty, now);
                // F18 自动滚动只在**真正出帧**的那一轮施加,见 match 之后的说明。
                let presented = matches!(action, RedrawAction::Present);
                match action {
                    RedrawAction::Present => {
                        if let Some(a) = &mut self.active {
                            let pane = self.conn.as_ref().map(|c| &c.pane);
                            let connected = self.conn.is_some();
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
                                }
                            });
                            // 与 host_key_view 同理:`self.pending_paste` 与
                            // `&mut self.ui` 是不相干字段,可同时借出。
                            let paste_view = self
                                .pending_paste
                                .as_deref()
                                .map(|text| crate::ui::paste::PasteView { text });
                            let repaint_delay = render_frame(
                                a,
                                pane,
                                &mut self.ui,
                                sessions,
                                store_available,
                                connected,
                                1, // 分屏(F30)未落地,恒 1 屏;F30 落地时这里要接真实 pane 数
                                host_key_view,
                                paste_view,
                            );
                            self.limiter.record_present(now);
                            // egui 侧已画出;下面若 egui 又要一帧会重新置脏。
                            self.ui_dirty = false;
                            if let Some(conn) = self.conn.as_mut() {
                                conn.pacer.mark_presented();
                                // 中央区(窗口减去 egui 菜单/状态栏)→ 终端网格(F34/T4)。
                                // central_px 由本帧 build_ui 在 egui 布局后写入 self.ui,
                                // 天然滞后一帧(首帧/未连接时用 resumed 里整窗口尺寸兜底)——
                                // 可接受,不为此再跑一次 egui(见 Task 说明)。
                                let (cols, rows) = shell::viewport::grid_dims(
                                    self.ui.central_px,
                                    (a.text.cell_w as u32, a.text.cell_h as u32),
                                    (1, 1),
                                );
                                if (cols, rows) != a.grid_dims {
                                    a.grid_dims = (cols, rows);
                                    conn.pane.emulator.resize(cols, rows);
                                    let _ = conn.ssh.resize(cols, rows); // T4
                                }
                            }
                            // 菜单动作(§4.2):断开回到 launcher 态 / 退出整个事件循环。
                            if self.ui.request_disconnect {
                                self.ui.request_disconnect = false;
                                self.conn = None;
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
                        self.next_frame_at = None;
                        event_loop.set_control_flow(ControlFlow::Wait);
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
                    if let Some(conn) = self.conn.as_mut() {
                        conn.pane.emulator.scroll(Scroll::Delta(lines));
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
                // self.active/self.conn/self.ui 的借用都已释放,才能拿 `&mut
                // self.store`(egui 闭包里借不到它,只能在这里事后统一施加)。
                if self.ui.delete_request.is_some() || self.ui.save_request.is_some() {
                    // keyring/TOML 是同步 IO,在事件回调里可能阻塞(Windows 凭据管理器
                    // 偶发几百 ms),打点让看门狗能指认。
                    diag::mark(diag::Stage::StoreIo);
                }
                if let Some(id) = self.ui.delete_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        if let Err(e) = store.delete(id).and_then(|_| store.save()) {
                            self.ui.last_error = Some(format!("删除失败:{e}"));
                        }
                    }
                }
                if let Some(save) = self.ui.save_request.take() {
                    if let Some(store) = self.store.as_mut() {
                        let now = time::OffsetDateTime::now_utc()
                            .format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default();
                        let r = match save.editing_id {
                            Some(id) => store
                                .update(id, save.draft, &now)
                                .and_then(|_| store.save()),
                            None => {
                                store.add(save.draft, &now);
                                store.save()
                            }
                        };
                        if let Err(e) = r {
                            self.ui.last_error = Some(format!("保存失败:{e}"));
                        }
                    }
                }
                // 「选择…」私钥文件:同样是 egui 闭包只记意图、这里才施加。
                if std::mem::take(&mut self.ui.pick_key_request) && !self.key_picker_busy {
                    self.key_picker_busy = true;
                    self.spawn_key_picker();
                }
                // 连接:双击行 / 点「连接」。spawn_connect 是 &self 方法,必须在
                // store 的 &mut 借用结束后调(下面 `self.store.as_ref()` 的临时借
                // 用在 match 表达式求值完就释放,故可紧接着调 self.spawn_connect)。
                if let Some(id) = self.ui.connect_request.take() {
                    match self.store.as_ref().map(|s| s.ssh_config_for(id)) {
                        Some(Ok(cfg)) => {
                            // 用户主动发起的连接是交互态,不该继承 CLI 直连的
                            // exit(1) 语义(复核 #1)。
                            self.cli_direct = false;
                            self.spawn_connect(cfg);
                        }
                        Some(Err(e)) => self.ui.last_error = Some(e.to_string()),
                        None => {}
                    }
                }
                // F3:主机密钥弹窗的回答。record + save 必须在 GUI 线程做——
                // store 是同步 IO,而且失败要能落进 last_error 让用户看见。
                if let Some(accept) = self.ui.host_key_reply.take() {
                    if let Some(prompt) = self.pending_host_key.take() {
                        self.host_key_since = None;
                        if accept {
                            diag::mark(diag::Stage::StoreIo);
                            // 与 host_key.rs 同源:GUI 线程 panic 比 tokio 线程更糟
                            // (直接崩窗口),锁中毒时恢复而不是 expect。
                            let mut kh = self.known_hosts.lock().unwrap_or_else(|e| e.into_inner());
                            kh.record(
                                &prompt.host,
                                HostKeyEntry {
                                    algo: prompt.algo.clone(),
                                    fingerprint: prompt.fingerprint.clone(),
                                },
                            );
                            // 落盘失败不阻断本次连接:指纹已在内存表里,连接照常;
                            // 代价只是下次启动会再问一遍。last_error 的展示位可能
                            // 随后被别的事件(如 ConnectOk 关掉会话管理器)挪走,
                            // 磁盘日志(ADR-008)是这类静默失败的兜底取证手段。
                            if let Err(e) = kh.save() {
                                crate::logx::line(&format!("主机指纹落盘失败:{e}"));
                                self.ui.last_error =
                                    Some(format!("主机指纹未能保存:{e}(本次连接不受影响)"));
                            }
                        }
                        // 送回握手线程。Err = 对端已走(超时/断开),没什么可做的。
                        let _ = prompt.reply.send(accept);
                    }
                }
                // F18:粘贴确认弹窗的回答。放在这里而不是 egui 闭包里——发送要
                // `&mut self.conn`,闭包里借不到(与会话管理器/主机密钥同构)。
                if let Some(accept) = self.ui.paste_reply.take() {
                    if let Some(text) = self.pending_paste.take() {
                        if accept {
                            self.send_paste(&text);
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

/// 一帧渲染:先跑 egui(菜单栏 + 状态栏,§4.2),再(终端态时)叠加背景色块 + 文字
/// 前景趟。返回 egui 想要的下次重绘时间(`Duration::MAX` = 不需要);调用方据此走
/// T3/T7 的 `next_frame_at`/`WaitUntil`,不会无条件 `request_redraw`。GPU 胶水,无单测。
#[allow(clippy::too_many_arguments)] // 计划(Task 9)明确要求的签名;拆结构体属于范围外重构。
fn render_frame(
    a: &mut Active,
    pane: Option<&Pane>,
    ui_state: &mut crate::ui::UiState,
    sessions: &[mullion_store::SessionRecord],
    store_available: bool,
    connected: bool,
    panes: usize,
    host_key: Option<crate::ui::host_key::HostKeyView<'_>>,
    paste: Option<crate::ui::paste::PasteView<'_>>,
) -> std::time::Duration {
    diag::count_frame();
    // --- egui:每帧都跑,launcher 态(pane=None)也要画菜单/状态栏。---
    diag::mark(diag::Stage::EguiRun);
    let raw_input = a.egui_state.take_egui_input(&a.window);
    let full_output = a.egui_ctx.run(raw_input, |ctx| {
        crate::ui::build_ui(
            ctx,
            &MULLION_DARK,
            ui_state,
            sessions,
            store_available,
            connected,
            panes,
            host_key,
            paste,
        );
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

    // --- 终端趟:仅 pane 存在(终端态)才生成 quads/prepare 文字;launcher 态
    // (pane=None)没有终端可画,跳过,只画上面的 egui。---
    let terminal_draw = match pane {
        Some(pane) => {
            diag::mark(diag::Stage::TextPrepare);
            let snap = pane.emulator.snapshot();
            let res = glyphon::Resolution {
                width: a.gpu.config.width,
                height: a.gpu.config.height,
            };
            // 终端整体平移到 egui 中央区(菜单栏之下)。origin 由本帧上面的
            // `build_ui` 刚写入,是**同帧**新鲜值(不像 central_px 要等到 present
            // 之后才被 grid_dims 消费而滞后一帧)。
            let origin = ui_state.central_origin_px;
            let quads = quads_for(
                &snap,
                origin,
                a.text.cell_w,
                a.text.cell_h,
                theme::term_default_colors(&MULLION_DARK),
            );
            // 渲染路径不许 panic:prepare 失败(如长会话把图集喂满 AtlasFull)记录并
            // 跳过整帧(含 egui),与 Task 3 之前的行为一致——不拖垮整个 GUI。
            if let Err(e) = a
                .text
                .prepare(&a.gpu.device, &a.gpu.queue, &snap, origin, res)
            {
                log::warn!(target: "mullion", "glyphon prepare 失败,跳过本帧: {e:?}");
                diag::count_skipped();
                return std::time::Duration::MAX;
            }
            let inst = a.gpu.quad_instances(&quads);
            Some((inst, quads.len() as u32))
        }
        None => None,
    };

    diag::mark(diag::Stage::Acquire);
    let frame = match a.gpu.surface.get_current_texture() {
        Ok(f) => f,
        Err(wgpu::SurfaceError::Timeout) => {
            log::warn!(target: "mullion", "wgpu get_current_texture 超时,跳过本帧");
            diag::count_skipped();
            return std::time::Duration::MAX;
        }
        Err(e @ (wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated)) => {
            log::warn!(target: "mullion", "wgpu surface {e:?},重新 configure 后跳过本帧");
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            diag::count_skipped();
            return std::time::Duration::MAX;
        }
        Err(wgpu::SurfaceError::OutOfMemory) => {
            log::error!(target: "mullion", "wgpu get_current_texture OutOfMemory,跳过本帧");
            diag::count_skipped();
            return std::time::Duration::MAX;
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

    repaint_delay
}

#[cfg(test)]
mod tests {
    use crate::frame::FrameLimiter;
    use crate::reflow::{reflow, ResizeSink};
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
}
