//! 分屏工作区(F30–F35):多 pane 状态机 + 几何 + 布局预设。
//!
//! 零 winit/wgpu/egui —— 状态机部分可纯单测,这是本切片能在无头容器里验证
//! 布局与 window_change 行为的前提。

pub mod geom;
pub mod preset;

pub use geom::{layout_geometry, PaneGeom, PxRect, GAP_PX, TITLE_BAR_PX};
pub use preset::{icon_cells, next_focus, plan_preset, preset_tree, Preset, PresetPlan};

/// pane 的连接状态(§6.3)。断开的 pane 内容保留、可滚可复制,只是不再收发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneStatus {
    Live,
    Disconnected,
}

use std::sync::Arc;

use mullion_core::layout::{close_pane, leaves, split_pane, Dir, Node, PaneId};
use mullion_ssh::session::{ClientHandler, SshSession, TrySendErr};
use mullion_term::emulator::Emulator;
use russh::client::Handle;
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
}

impl PtyWriter for SshSession {
    fn write(&self, bytes: Vec<u8>) -> Result<(), TrySendErr> {
        SshSession::write(self, bytes)
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), TrySendErr> {
        SshSession::resize(self, cols, rows)
    }
}

/// 一条**已建立**的 SSH 连接。B2-a 里恒只有一个;数据模型按 N host × M channel
/// 设计,是为了 B2-b 的"换主机"能原地加而不用推翻结构(§4.2)。
pub struct HostConn {
    /// 状态栏/标题条上显示的名字(会话名或 user@host)。
    pub label: String,
    /// `user@host:port`,标题条副标题用。
    pub addr: String,
    /// `Arc` 是必须的:russh 的 `Handle` 没实现 `Clone`,只有 `Drop`(释放即断连)。
    pub handle: Arc<Handle<ClientHandler>>,
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
    /// 上次发出去的 (cols, rows)。F34 只在这个值变化时才发 window_change ——
    /// 每帧无脑发会把远端 SIGWINCH 刷爆,tmux 里的 TUI 不停重排。
    pub last_grid: (u16, u16),
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

    /// 异步 `open_pty` 完成后把 pane 挂进来(id 由 [`Workspace::apply_preset`] 预分配)。
    pub fn attach_pane(&mut self, pane: PaneState) {
        self.next_id = self.next_id.max(pane.id.0 + 1);
        self.panes.retain(|p| p.id != pane.id);
        self.panes.push(pane);
    }

    /// 关闭一个 pane(F31):树上兄弟顶替,`PaneState` 一并丢弃(channel 随之关闭)。
    /// 最后一个 pane 不可关,返回 `false` 且什么都不动。
    pub fn close_pane(&mut self, id: PaneId) -> bool {
        if !close_pane(&mut self.tree, id) {
            return false;
        }
        self.panes.retain(|p| p.id != id);
        self.focus = next_focus(self.focus, &leaves(&self.tree));
        true
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
                        p.status = PaneStatus::Disconnected;
                        break;
                    }
                }
            }
            if inbound.is_empty() {
                continue;
            }
            let out = session_pump::pump(&mut p.emulator, &inbound);
            if !out.is_empty() {
                let _ = p.pty.write(out);
            }
        }
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

#[cfg(test)]
mod tests {
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
    }

    struct Probe {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        resizes: Arc<Mutex<Vec<(u16, u16)>>>,
        resize_fails: Arc<Mutex<bool>>,
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
    }

    /// 造一个挂着 FakePty 的 pane,并把它的输入端 / 观测端一起返回。
    fn fake_pane(id: u32) -> (PaneState, Probe) {
        let pty = FakePty::default();
        let probe_writes = pty.writes.clone();
        let probe_resizes = pty.resizes.clone();
        let probe_resize_fails = pty.resize_fails.clone();
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
                last_grid: (80, 24),
            },
            Probe {
                writes: probe_writes,
                resizes: probe_resizes,
                resize_fails: probe_resize_fails,
                tx,
            },
        )
    }

    fn ws_with(n: u32) -> (Workspace, Vec<Probe>) {
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
}
