//! App:winit ApplicationHandler<UserEvent>。拥有窗口/GPU/文字层/pane/SSH 会话/运行时,
//! 每帧「排空 rx → feed emu → 回写 PtyWrite(T1)」,GPU present 受帧率(T3)与同步块(T2)双闸。

use std::sync::Arc;
use std::time::Instant;

use mullion_core::layout::PaneId;
use mullion_ssh::session::SshSession;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Receiver;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::frame::{FrameLimiter, RedrawAction};
use crate::gpu::{quads_for, Gpu};
use crate::pane::Pane;
use crate::render::SyncFramePacer;
use crate::text::TextLayer;
use crate::{grid, input, session_pump};

/// 唤醒重绘的用户事件(ssh io_task 经注入的 wake 回调触发)。
#[derive(Debug, Clone, Copy)]
pub enum UserEvent {
    Wake,
}

/// 窗口出现后才建的 GPU 相关状态。
struct Active {
    window: Arc<Window>,
    gpu: Gpu,
    text: TextLayer,
    grid_dims: (u16, u16),
}

pub struct App {
    _runtime: Runtime,
    ssh: SshSession,
    rx: Receiver<Vec<u8>>,
    pane: Pane,
    pacer: SyncFramePacer,
    limiter: FrameLimiter,
    start: Instant,
    mods: ModifiersState,
    kitty: bool,
    active: Option<Active>,
    /// 被 `RedrawAction::Throttle` 挡住时记的到点时刻;`about_to_wait` 据此在
    /// deadline 到达后补一次 `request_redraw`,而不是靠陈旧 `WaitUntil` 忙转(T3/N3)。
    next_frame_at: Option<Instant>,
}

/// 显示字号(磅 / point)。渲染时按窗口 DPI 缩放成物理像素。
/// TODO:与字体族一起做成可配置(见 spec F21)。
const FONT_POINT_SIZE: f32 = 10.0;

impl App {
    pub fn new(runtime: Runtime, ssh: SshSession, rx: Receiver<Vec<u8>>) -> Self {
        Self {
            _runtime: runtime,
            ssh,
            rx,
            pane: Pane::new(PaneId(1), 80, 24),
            pacer: SyncFramePacer::new(),
            limiter: FrameLimiter::new(16), // ~60fps(T3)
            start: Instant::now(),
            mods: ModifiersState::empty(),
            kitty: false, // MVP 未协商 Kitty,走优雅退化(T6)
            active: None,
            next_frame_at: None,
        }
    }

    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.active.is_some() {
            return;
        }
        let window = Arc::new(
            event_loop
                .create_window(Window::default_attributes().with_title("mullion"))
                .expect("create_window"),
        );
        let gpu = Gpu::new(window.clone(), self._runtime.handle());
        // 字号 10pt,按窗口 DPI 缩放成物理像素(inner_size 是物理像素,须一致):
        // px = pt * (96*scale/72)。Windows 常见 125%/150% 缩放下才不会过小。
        // TODO:字体/字号做成可配置 + 跟随 ScaleFactorChanged 动态更新(见 spec F21)。
        let scale = window.scale_factor() as f32;
        let font_px = FONT_POINT_SIZE * scale * 96.0 / 72.0;
        let text = TextLayer::new(&gpu.device, &gpu.queue, gpu.config.format, font_px);
        let size = window.inner_size();
        let (cols, rows) = grid::grid_size_for(size.width, size.height, text.cell_w, text.cell_h);
        self.pane.emulator.resize(cols, rows);
        let _ = self.ssh.resize(cols, rows); // 初始 window_change 校正到真实尺寸(T4)
        self.active = Some(Active {
            window,
            gpu,
            text,
            grid_dims: (cols, rows),
        });
    }

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: UserEvent) {
        if let Some(a) = &self.active {
            a.window.request_redraw();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::ModifiersChanged(m) => self.mods = m.state(),
            WindowEvent::Resized(size) => {
                if let Some(a) = &mut self.active {
                    a.gpu.resize(size.width, size.height);
                    let (cols, rows) =
                        grid::grid_size_for(size.width, size.height, a.text.cell_w, a.text.cell_h);
                    if (cols, rows) != a.grid_dims {
                        a.grid_dims = (cols, rows);
                        // 单 pane MVP 直接 resize;多 pane 的 reflow(ResizeSink)留给 F4 分屏。
                        self.pane.emulator.resize(cols, rows);
                        let _ = self.ssh.resize(cols, rows); // T4
                    }
                    a.window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state == ElementState::Pressed {
                    if let Some((key, mods)) = input::translate_key(&event, self.mods) {
                        let bytes = mullion_term::keymap::encode_key(key, mods, self.kitty);
                        // `let _` 全文件都这样:写/resize 失败(断线等)没有用户提示、
                        // 无重连。断线感知与重连是 S3,后续 spec,这里不做。
                        let _ = self.ssh.write(bytes);
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                // 1. 排空 rx(永远做:保 T1 应答流动 + T3 解耦)
                let mut inbound = Vec::new();
                while let Ok(bytes) = self.rx.try_recv() {
                    inbound.push(bytes);
                }
                for b in &inbound {
                    self.pacer.feed(b); // T2:探测同步块
                }
                // 2. feed emu + 回写 PtyWrite(T1 红线)
                let out = session_pump::pump(&mut self.pane.emulator, &inbound);
                if !out.is_empty() {
                    let _ = self.ssh.write(out);
                }
                // 3. present 受帧率(T3)与同步块(T2)双闸。`plan` 是纯决策,
                // 三支都显式复位 control_flow——Throttle 靠 about_to_wait 到点补画,
                // 不在这里 request_redraw,否则陈旧 WaitUntil 过期后每轮零延迟
                // ResumeTimeReached 会忙转空转满 CPU(T3/N3 红线)。
                let dirty = self.pacer.should_present();
                let now = self.now_ms();
                match self.limiter.plan(dirty, now) {
                    RedrawAction::Present => {
                        if let Some(a) = &mut self.active {
                            render_frame(a, &self.pane);
                        }
                        self.limiter.record_present(now);
                        self.pacer.mark_presented();
                        self.next_frame_at = None;
                        event_loop.set_control_flow(ControlFlow::Wait);
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
    }
}

/// 一帧渲染:背景色块趟 + 文字前景趟。GPU 胶水,无单测。
fn render_frame(a: &mut Active, pane: &Pane) {
    // 每帧先 trim:清掉上一帧的 glyphs_in_use,让本帧 prepare 能按需淘汰旧字形。
    // 必须在 prepare/get_current_texture 的 early-return 之前——挪到函数末尾会导致
    // 一旦 AtlasFull 触发提前 return,trim 永远到不了,图集永远不被清理,
    // 下一帧 prepare 还是 AtlasFull,画面冻在最后一次成功帧且无法自愈。
    // trim 只清 in_use 标记不删纹理,首帧对空图集是 no-op,正常帧语义不变。
    a.text.trim();

    let snap = pane.emulator.snapshot();
    let res = glyphon::Resolution {
        width: a.gpu.config.width,
        height: a.gpu.config.height,
    };
    let quads = quads_for(
        &snap,
        a.text.cell_w,
        a.text.cell_h,
        mullion_term::palette::DEFAULT_BG,
    );
    let inst = a.gpu.quad_instances(&quads);
    // 渲染路径不许 panic:prepare 失败(如长会话把图集喂满 AtlasFull)记录并跳过本帧,
    // 不拖垮整个 GUI。
    if let Err(e) = a.text.prepare(&a.gpu.device, &a.gpu.queue, &snap, res) {
        eprintln!("glyphon prepare 失败,跳过本帧: {e:?}");
        return;
    }

    let frame = match a.gpu.surface.get_current_texture() {
        Ok(f) => f,
        Err(wgpu::SurfaceError::Timeout) => {
            eprintln!("wgpu get_current_texture 超时,跳过本帧");
            return;
        }
        Err(e @ (wgpu::SurfaceError::Lost | wgpu::SurfaceError::Outdated)) => {
            eprintln!("wgpu surface {e:?},重新 configure 后跳过本帧");
            a.gpu.surface.configure(&a.gpu.device, &a.gpu.config);
            return;
        }
        Err(wgpu::SurfaceError::OutOfMemory) => {
            eprintln!("wgpu get_current_texture OutOfMemory,跳过本帧");
            return;
        }
    };
    let view = frame
        .texture
        .create_view(&wgpu::TextureViewDescriptor::default());
    let mut enc = a
        .gpu
        .device
        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("frame"),
        });
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("main"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                        a: 1.0,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });
        a.gpu.draw_quads(&mut pass, &inst, quads.len() as u32); // 背景趟
                                                                // 前景趟:失败(如条目在 prepare 之后被图集淘汰)不 panic,记录并跳过文字层,
                                                                // 背景色块这帧仍照常提交。
        if let Err(e) = a.text.render(&mut pass) {
            eprintln!("glyphon render 失败,跳过本帧文字层: {e:?}");
        }
    }
    a.gpu.queue.submit(Some(enc.finish()));
    frame.present();
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
