//! GPU 层:背景/光标色块生成(纯,可测)+ wgpu 表面与色块管线(GPU 胶水,见 Task 8)。

use mullion_term::palette::DefaultColors;
use mullion_term::snapshot::GridSnapshot;

/// 一个实心色块(背景 / 光标),像素坐标(左上原点)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [u8; 3],
}

use crate::shell::workspace::PaneGeom;

/// 光标画法。多 pane 下必须区分:4 个 pane 同时亮 4 个实心光标的话,
/// 用户看不出键盘输入进了哪一块(§7.1)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorStyle {
    /// 焦点 pane:实心块。
    Block,
    /// 非焦点 pane:空心框。
    Hollow,
}

/// 一个 pane 的渲染输入。
pub struct PaneRender<'a> {
    pub geom: PaneGeom,
    pub snap: &'a GridSnapshot,
    pub focused: bool,
}

/// 空心光标的边框粗细(像素)。
const HOLLOW_PX: f32 = 1.0;

/// 从快照生成需要画的色块:bg ≠ 默认 的格 + 选中格(反色,F18)+ 可见光标(块状)。
/// 纯函数,可单测。
///
/// `origin` 是终端区左上角的窗口像素坐标(egui 菜单栏/状态栏之间的中央区)。
/// 网格坐标一律相对该原点:传 `(0.0, 0.0)` 得到纯网格坐标(测试用),实际渲染
/// 传中央区原点,否则第 0 行画在窗口顶端、被菜单栏盖住。文字层
/// (`text::TextLayer::prepare`)必须用**同一个** origin,不然底色和字会错位。
///
/// `defaults` 必须来自 `theme::term_default_colors`(F80 三处同源),不要直接传
/// `palette::DEFAULT_*`——那样主题一换就和 clear 色失配。
///
/// `cursor` 控制光标画法:焦点 pane 传 `Block`,其余 pane 传 `Hollow`(§7.1)。
pub fn quads_for(
    snap: &GridSnapshot,
    origin: (f32, f32),
    cell_w: f32,
    cell_h: f32,
    defaults: DefaultColors,
    cursor: CursorStyle,
) -> Vec<Quad> {
    let mut quads = Vec::new();
    for row in 0..snap.rows {
        for (col, cell) in snap.row(row).iter().enumerate() {
            if cell.spacer {
                continue;
            }
            // F18:选中格画反色底——用前景色当底,文字那趟同步改用 bg 色
            // (见 `text::row_to_spans`)。反色优先于下面「bg 是默认色就不画」
            // 的短路,否则选区在默认背景上完全看不见。
            let color = if cell.selected {
                cell.fg
            } else if cell.bg == defaults.bg {
                continue;
            } else {
                cell.bg
            };
            quads.push(Quad {
                x: origin.0 + col as f32 * cell_w,
                y: origin.1 + row as f32 * cell_h,
                w: cell.width.max(1) as f32 * cell_w,
                h: cell_h,
                color: [color.r, color.g, color.b],
            });
        }
    }
    if snap.cursor.visible {
        let x = origin.0 + snap.cursor.col as f32 * cell_w;
        let y = origin.1 + snap.cursor.row as f32 * cell_h;
        // MVP 光标用默认前景色。原本硬编码 0xcc,主题化后必须跟着走,
        // 否则新前景下光标是一块突兀的旧灰。
        let color = [defaults.fg.r, defaults.fg.g, defaults.fg.b];
        match cursor {
            CursorStyle::Block => quads.push(Quad {
                x,
                y,
                w: cell_w,
                h: cell_h,
                color,
            }),
            CursorStyle::Hollow => {
                let t = HOLLOW_PX;
                for q in [
                    Quad {
                        x,
                        y,
                        w: cell_w,
                        h: t,
                        color,
                    }, // 上
                    Quad {
                        x,
                        y: y + cell_h - t,
                        w: cell_w,
                        h: t,
                        color,
                    }, // 下
                    Quad {
                        x,
                        y,
                        w: t,
                        h: cell_h,
                        color,
                    }, // 左
                    Quad {
                        x: x + cell_w - t,
                        y,
                        w: t,
                        h: cell_h,
                        color,
                    }, // 右
                ] {
                    quads.push(q);
                }
            }
        }
    }
    quads
}

/// 把所有 pane 的色块合成一批(一次 draw call)。
///
/// 每个 pane 的原点取**自己的** `term_px`,不是整窗原点 —— 传错就会把 pane 2
/// 的底色画到 pane 1 上,症状是"字在新位置、底色还在老位置"。
/// 文字层(`text::prepare_panes`)必须用同一份 `PaneGeom`。
///
/// `grid_size_for`(`grid.rs`)把 `cols`/`rows` 夹到至少 `(1,1)`,即便 `term_px`
/// 已经窄/矮于一个整格。`quads_for` 按整格 `cell_w × cell_h` 画色块/光标,从不
/// 对照 `term_px` 裁——degenerate pane 下会画出自己的地盘之外,糊到邻居 pane
/// 头上。这里没有 wgpu 侧的 per-pane scissor(一次 draw call 画所有 pane 做不
/// 到),所以裁剪在 CPU 侧、按每个 pane 自己的 `term_px` 边界做。
pub fn quads_for_panes(
    panes: &[PaneRender<'_>],
    cell_w: f32,
    cell_h: f32,
    defaults: DefaultColors,
) -> Vec<Quad> {
    let mut out = Vec::new();
    for p in panes {
        let origin = (p.geom.term_px.x as f32, p.geom.term_px.y as f32);
        let style = if p.focused {
            CursorStyle::Block
        } else {
            CursorStyle::Hollow
        };
        let bounds = (
            origin.0,
            origin.1,
            origin.0 + p.geom.term_px.w as f32,
            origin.1 + p.geom.term_px.h as f32,
        );
        out.extend(
            quads_for(p.snap, origin, cell_w, cell_h, defaults, style)
                .into_iter()
                .filter_map(|q| clamp_quad_to_bounds(q, bounds)),
        );
    }
    out
}

/// 把 `q` 裁到 `(left, top, right, bottom)` 边界内;裁到宽或高 <= 0(quad 整个
/// 落在边界外)时返回 `None`,调用方应该跳过不画。
fn clamp_quad_to_bounds(q: Quad, (left, top, right, bottom): (f32, f32, f32, f32)) -> Option<Quad> {
    let x = q.x.max(left);
    let y = q.y.max(top);
    let w = (q.x + q.w).min(right) - x;
    let h = (q.y + q.h).min(bottom) - y;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(Quad {
        x,
        y,
        w,
        h,
        color: q.color,
    })
}

use std::sync::Arc;

use wgpu::util::DeviceExt;
use winit::window::Window;

/// wgpu 表面 + 设备 + 色块管线。GPU 胶水:无单测,守护=编译+起窗口。
pub struct Gpu {
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub config: wgpu::SurfaceConfiguration,
    quad_pipeline: wgpu::RenderPipeline,
    resolution_buf: wgpu::Buffer,
    resolution_bind: wgpu::BindGroup,
}

/// 传给着色器的每实例数据:像素矩形 + 归一化颜色。
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct QuadInstance {
    rect: [f32; 4],  // x, y, w, h(像素)
    color: [f32; 4], // r,g,b,1
}

impl Gpu {
    /// 用 `handle`(app 的 tokio 运行时)block_on wgpu 的 async 初始化。
    pub fn new(window: Arc<Window>, handle: &tokio::runtime::Handle) -> Self {
        let size = window.inner_size();
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::default());
        let surface = instance
            .create_surface(window.clone())
            .expect("create_surface");
        let adapter = handle
            .block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: Some(&surface),
                force_fallback_adapter: false,
            }))
            .expect("无可用 GPU adapter");
        // GPU/驱动身份写进自己的日志。上次真机卡死时是靠 Windows 事件日志里
        // Explorer 崩在 amdxx64.dll 才知道驱动版本的——那条路不可靠,自己记。
        let info = adapter.get_info();
        log::info!(
            target: "mullion",
            "GPU: {} [{:?}] backend={:?} vendor=0x{:04x} device=0x{:04x} driver={} {}",
            info.name, info.device_type, info.backend, info.vendor, info.device,
            info.driver, info.driver_info,
        );
        let (device, queue) = handle
            .block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
            .expect("request_device");
        // 设备级故障自报:TDR / 驱动重置 / 校验层错误由 wgpu 直接告诉我们,
        // 不用再从「Explorer 崩了」反推。回调在 wgpu 内部线程调用,只写日志。
        device.on_uncaptured_error(Box::new(|e| {
            log::error!(target: "mullion", "wgpu 未捕获错误: {e}");
        }));
        device.set_device_lost_callback(|reason, msg| {
            log::error!(target: "mullion", "wgpu 设备丢失({reason:?}): {msg}");
        });

        let caps = surface.get_capabilities(&adapter);
        let format = caps
            .formats
            .iter()
            .copied()
            .find(|f| f.is_srgb())
            .unwrap_or(caps.formats[0]);
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode: wgpu::PresentMode::Fifo, // vsync,配合帧率闸 T3
            alpha_mode: caps.alpha_modes[0],
            view_formats: vec![],
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);
        log::info!(
            target: "mullion",
            "surface: {}x{} format={:?} present={:?} alpha={:?} latency={}",
            config.width, config.height, config.format, config.present_mode,
            config.alpha_mode, config.desired_maximum_frame_latency,
        );

        // resolution uniform(vec2<f32>,补齐到 16 字节)。用 config 同款 max(1) 裁剪值,
        // 避免窗口初始 0×0 时着色器里 px / resolution 除零出 NaN。
        let resolution_buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("resolution"),
            contents: bytemuck::cast_slice(&[config.width as f32, config.height as f32, 0.0, 0.0]),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("res-layout"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let resolution_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("res-bind"),
            layout: &bind_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: resolution_buf.as_entire_binding(),
            }],
        });

        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad-shader"),
            source: wgpu::ShaderSource::Wgsl(QUAD_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad-layout"),
            bind_group_layouts: &[&bind_layout],
            push_constant_ranges: &[],
        });
        let quad_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<QuadInstance>() as u64,
                    step_mode: wgpu::VertexStepMode::Instance,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4],
                }],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleStrip,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview: None,
            cache: None,
        });

        Self {
            surface,
            device,
            queue,
            config,
            quad_pipeline,
            resolution_buf,
            resolution_bind,
        }
    }

    /// 表面 resize(窗口尺寸变)。
    pub fn resize(&mut self, w: u32, h: u32) {
        self.config.width = w.max(1);
        self.config.height = h.max(1);
        log::debug!(target: "mullion", "surface configure {}x{}", self.config.width, self.config.height);
        self.surface.configure(&self.device, &self.config);
        // 用钳制后的 config 值(与 new() 一致),不是未钳制的 w/h——Windows 最小化会送
        // 一次 Resized(0,0),config 被钳到 1×1 但若这里写 (0,0) 进 uniform,着色器
        // px / resolution 出 NaN,该帧几何全坏。
        self.queue.write_buffer(
            &self.resolution_buf,
            0,
            bytemuck::cast_slice(&[
                self.config.width as f32,
                self.config.height as f32,
                0.0,
                0.0,
            ]),
        );
    }

    /// 把色块转成实例缓冲(每帧一次性上传)。
    pub fn quad_instances(&self, quads: &[Quad]) -> wgpu::Buffer {
        let data: Vec<QuadInstance> = quads
            .iter()
            .map(|q| QuadInstance {
                rect: [q.x, q.y, q.w, q.h],
                color: [
                    q.color[0] as f32 / 255.0,
                    q.color[1] as f32 / 255.0,
                    q.color[2] as f32 / 255.0,
                    1.0,
                ],
            })
            .collect();
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: Some("quad-instances"),
                contents: bytemuck::cast_slice(&data),
                usage: wgpu::BufferUsages::VERTEX,
            })
    }

    /// 在已开的 render pass 里画所有色块。
    pub fn draw_quads<'a>(
        &'a self,
        pass: &mut wgpu::RenderPass<'a>,
        inst: &'a wgpu::Buffer,
        n: u32,
    ) {
        if n == 0 {
            return;
        }
        pass.set_pipeline(&self.quad_pipeline);
        pass.set_bind_group(0, &self.resolution_bind, &[]);
        pass.set_vertex_buffer(0, inst.slice(..));
        pass.draw(0..4, 0..n);
    }
}

const QUAD_WGSL: &str = r#"
@group(0) @binding(0) var<uniform> resolution: vec4<f32>;

struct VsOut { @builtin(position) pos: vec4<f32>, @location(0) color: vec4<f32> };

// surface 格式是 sRGB(见 Gpu::new 的 is_srgb 挑选),硬件会把着色器输出当**线性**
// 值再编码成 sRGB。所以这里必须先把 sRGB 分量转成线性,否则画出来比实际亮一截。
// egui(egui.wgsl 的 linear_from_gamma_rgb)与 glyphon(shader.wgsl 的
// srgb_to_linear)都这么做;我们不做就会和它们对不上。放顶点着色器:每个
// quad 4 次,比逐像素便宜。
fn srgb_to_linear(c: vec3<f32>) -> vec3<f32> {
    let cutoff = c < vec3<f32>(0.04045);
    let lower = c / vec3<f32>(12.92);
    let higher = pow((c + vec3<f32>(0.055)) / vec3<f32>(1.055), vec3<f32>(2.4));
    return select(higher, lower, cutoff);
}

@vertex
fn vs_main(@builtin(vertex_index) vi: u32,
           @location(0) rect: vec4<f32>,
           @location(1) color: vec4<f32>) -> VsOut {
    // TriangleStrip 四角:(0,0)(1,0)(0,1)(1,1)
    let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
    let px = rect.xy + corner * rect.zw;        // 像素坐标(左上原点)
    let ndc = vec2<f32>(
        px.x / resolution.x * 2.0 - 1.0,
        1.0 - px.y / resolution.y * 2.0,        // y 翻转
    );
    var out: VsOut;
    out.pos = vec4<f32>(ndc, 0.0, 1.0);
    out.color = vec4<f32>(srgb_to_linear(color.rgb), color.a);
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return in.color; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::workspace::PxRect;
    use mullion_term::palette::DefaultColors;
    use mullion_term::snapshot::{Cursor, CursorShape, Rgb, SnapCell};

    fn snap_1x1(bg: Rgb) -> GridSnapshot {
        GridSnapshot {
            cols: 1,
            rows: 1,
            cells: vec![SnapCell {
                ch: ' ',
                fg: Rgb::new(0xcc, 0xcc, 0xcc),
                bg,
                width: 1,
                spacer: false,
                selected: false,
            }],
            cursor: Cursor {
                row: 0,
                col: 0,
                visible: false,
                shape: CursorShape::Beam,
                blinking: true,
            },
        }
    }

    #[test]
    fn origin_shifts_every_quad_so_first_row_clears_the_menu_bar() {
        // 真机症状:第 0 行画在窗口顶端(y=0),被 egui 顶部菜单栏整条盖住,
        // 用户看不到登录横幅第一行。行数已按中央区算过,漏的是这一下平移。
        let mut snap = snap_1x1(Rgb::new(205, 0, 0));
        snap.cursor.visible = true;
        let quads = quads_for(
            &snap,
            (0.0, 24.0),
            10.0,
            20.0,
            DefaultColors::default(),
            CursorStyle::Block,
        );
        assert_eq!(quads.len(), 2, "一个背景块 + 一个光标块");
        for q in &quads {
            assert_eq!(q.y, 24.0, "第 0 行必须落在中央区顶端,不是窗口顶端");
        }
        // 尺寸不跟着平移变——平移只动位置。
        assert_eq!(quads[0].h, 20.0);
    }

    #[test]
    fn default_bg_cell_makes_no_quad() {
        let snap = snap_1x1(Rgb::new(0, 0, 0)); // == DEFAULT_BG
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors::default(),
            CursorStyle::Block,
        );
        assert!(quads.is_empty(), "默认背景不该产生色块(省 GPU)");
    }

    #[test]
    fn colored_bg_cell_makes_quad_at_pixel() {
        let snap = snap_1x1(Rgb::new(205, 0, 0));
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors::default(),
            CursorStyle::Block,
        );
        assert_eq!(quads.len(), 1);
        assert_eq!(
            quads[0],
            Quad {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 20.0,
                color: [205, 0, 0]
            }
        );
    }

    #[test]
    fn visible_cursor_adds_block_quad() {
        let mut snap = snap_1x1(Rgb::new(0, 0, 0));
        snap.cursor.visible = true;
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors::default(),
            CursorStyle::Block,
        );
        assert_eq!(quads.len(), 1, "仅光标块(默认背景无块)");
        assert_eq!(quads[0].w, 10.0);
    }

    fn snap_selected_1x1(fg: Rgb, bg: Rgb) -> GridSnapshot {
        GridSnapshot {
            cols: 1,
            rows: 1,
            cells: vec![SnapCell {
                ch: 'a',
                fg,
                bg,
                width: 1,
                spacer: false,
                selected: true,
            }],
            cursor: Cursor {
                row: 0,
                col: 0,
                visible: false,
                shape: CursorShape::Beam,
                blinking: true,
            },
        }
    }

    #[test]
    fn selected_cell_is_inverted_even_on_default_background() {
        // 反色必须优先于「bg 是默认色就不画」这条既有短路,否则在默认背景上
        // (也就是绝大多数情况)选区完全看不见。
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let snap = snap_selected_1x1(fg, Rgb::new(0, 0, 0));
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors::default(),
            CursorStyle::Block,
        );
        assert_eq!(quads.len(), 1, "选中格必须画底色块");
        assert_eq!(quads[0].color, [0xcc, 0xcc, 0xcc], "底色应换成前景色");
    }

    /// 守 F80 前置:surface 是 sRGB 格式(`is_srgb()` 挑的),着色器输出会被硬件
    /// 当线性值再编码。不转换的话,同一个 token 在 egui(自己转了)和终端色块
    /// (没转)里会画成两个颜色——底色非黑之后肉眼可见。
    /// 数值正确性只能人眼验;这里守的是「转换没被后来的重构删掉」。
    #[test]
    fn quad_shader_converts_srgb_to_linear() {
        assert!(
            QUAD_WGSL.contains("fn srgb_to_linear"),
            "quad 着色器缺 sRGB→线性 转换,终端色块会比 egui 外壳亮一截"
        );
        assert!(
            QUAD_WGSL.contains("srgb_to_linear(color.rgb)"),
            "srgb_to_linear 定义了但没用在顶点色上"
        );
    }

    /// 计划期发现:光标色原本硬编码 [0xcc,0xcc,0xcc],不引用任何常量,
    /// 改主题前景后会留一块旧灰。必须跟着 DefaultColors.fg 走。
    #[test]
    fn cursor_uses_injected_default_fg() {
        let fg = Rgb::new(0xe4, 0xe6, 0xf0);
        let bg = Rgb::new(0x14, 0x16, 0x1f);
        let mut snap = snap_1x1(bg);
        snap.cursor = Cursor {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Beam,
            blinking: true,
        };
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors { fg, bg },
            CursorStyle::Block,
        );
        let cursor = quads.last().expect("光标可见时应有一个 quad");
        assert_eq!(cursor.color, [0xe4, 0xe6, 0xf0], "光标色应取注入的默认前景");
    }

    /// 非黑主题底色下,默认背景格同样不画 quad(靠 clear 色透出)。
    #[test]
    fn default_bg_cell_makes_no_quad_on_themed_bg() {
        let bg = Rgb::new(0x14, 0x16, 0x1f);
        let snap = snap_1x1(bg);
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors {
                fg: Rgb::new(0xe4, 0xe6, 0xf0),
                bg,
            },
            CursorStyle::Block,
        );
        assert!(
            quads.is_empty(),
            "背景 == 主题默认背景 的格子不该画 quad,否则白扔一块画面"
        );
    }

    /// §7.1:非焦点 pane 的光标画空心框。不区分的话 4 屏会同时亮 4 个实心光标,
    /// 用户看不出键盘输入到底进了哪一块。
    #[test]
    fn hollow_cursor_draws_a_frame_not_a_block() {
        let mut snap = snap_1x1(Rgb::new(0, 0, 0));
        snap.cursor.visible = true;
        let quads = quads_for(
            &snap,
            (0.0, 0.0),
            10.0,
            20.0,
            DefaultColors::default(),
            CursorStyle::Hollow,
        );
        assert_eq!(quads.len(), 4, "空心光标 = 上下左右四条边");
        // 每条边都是 1px 细的,且都贴着这一格的边界。
        for q in &quads {
            assert!(q.w == 1.0 || q.h == 1.0, "边框条应有一维是 1px: {q:?}");
            assert!(q.x >= 0.0 && q.x + q.w <= 10.0);
            assert!(q.y >= 0.0 && q.y + q.h <= 20.0);
        }
        // 中心不能被填掉,否则跟实心块没区别。
        assert!(
            !quads.iter().any(|q| q.w > 2.0 && q.h > 2.0),
            "空心光标里混进了实心块"
        );
        // 四条边必须分别落在上/下/左/右四个不同位置——不能有重复(比如把「上」
        // 复制 4 遍,前面几条断言照样通过)。
        let cell_w = 10.0_f32;
        let cell_h = 20.0_f32;
        let t = 1.0_f32;
        let mut expected = vec![
            (0.0, 0.0, cell_w, t),        // 上
            (0.0, cell_h - t, cell_w, t), // 下
            (0.0, 0.0, t, cell_h),        // 左
            (cell_w - t, 0.0, t, cell_h), // 右
        ];
        let mut actual: Vec<_> = quads.iter().map(|q| (q.x, q.y, q.w, q.h)).collect();
        actual.sort_by(|a, b| a.partial_cmp(b).unwrap());
        expected.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(
            actual, expected,
            "四条边应分别落在上/下/左/右四个不同位置,不能有重复"
        );
    }

    #[test]
    fn invisible_cursor_draws_nothing_in_either_style() {
        let snap = snap_1x1(Rgb::new(0, 0, 0));
        for style in [CursorStyle::Block, CursorStyle::Hollow] {
            let quads = quads_for(
                &snap,
                (0.0, 0.0),
                10.0,
                20.0,
                DefaultColors::default(),
                style,
            );
            assert!(quads.is_empty(), "光标不可见时不该画: {style:?}");
        }
    }

    /// 每个 pane 用**自己的** term_px 原点。传整窗原点的话,pane 2 的底色会画到
    /// pane 1 的地盘上 —— 症状是"字在新位置、底色还在老位置"。
    #[test]
    fn each_pane_uses_its_own_term_origin() {
        let a = snap_1x1(Rgb::new(205, 0, 0));
        let b = snap_1x1(Rgb::new(0, 205, 0));
        let geom = |id: u32, x: u32| PaneGeom {
            id: mullion_core::layout::PaneId(id),
            px: PxRect {
                x,
                y: 100,
                w: 400,
                h: 600,
            },
            title_px: PxRect {
                x,
                y: 100,
                w: 400,
                h: 32,
            },
            term_px: PxRect {
                x,
                y: 132,
                w: 400,
                h: 568,
            },
            grid: (40, 28),
        };
        let panes = [
            PaneRender {
                geom: geom(1, 0),
                snap: &a,
                focused: true,
            },
            PaneRender {
                geom: geom(2, 400),
                snap: &b,
                focused: false,
            },
        ];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert_eq!(quads.len(), 2);
        assert_eq!((quads[0].x, quads[0].y), (0.0, 132.0));
        assert_eq!((quads[1].x, quads[1].y), (400.0, 132.0));
        assert_eq!(quads[1].color, [0, 205, 0]);
    }

    #[test]
    fn only_the_focused_pane_gets_a_solid_cursor() {
        let mut a = snap_1x1(Rgb::new(0, 0, 0));
        a.cursor.visible = true;
        let mut b = snap_1x1(Rgb::new(0, 0, 0));
        b.cursor.visible = true;
        let geom = |id: u32, x: u32| PaneGeom {
            id: mullion_core::layout::PaneId(id),
            px: PxRect {
                x,
                y: 0,
                w: 400,
                h: 600,
            },
            title_px: PxRect {
                x,
                y: 0,
                w: 400,
                h: 0,
            },
            term_px: PxRect {
                x,
                y: 0,
                w: 400,
                h: 600,
            },
            grid: (40, 30),
        };
        let panes = [
            PaneRender {
                geom: geom(1, 0),
                snap: &a,
                focused: true,
            },
            PaneRender {
                geom: geom(2, 400),
                snap: &b,
                focused: false,
            },
        ];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        let (pane0, pane1): (Vec<&Quad>, Vec<&Quad>) = quads.iter().partition(|q| q.x < 400.0);
        assert_eq!(pane0.len(), 1, "焦点 pane(x<400)只应有 1 个实心光标块");
        assert_eq!(
            (pane0[0].w, pane0[0].h),
            (10.0, 20.0),
            "焦点光标应是整格实心块,不是边框条"
        );
        assert_eq!(pane1.len(), 4, "非焦点 pane(x>=400)应是 4 条边框");
        assert!(
            pane1.iter().all(|q| q.w == 1.0 || q.h == 1.0),
            "非焦点光标每条都应是 1px 边框,不是实心块"
        );
    }

    /// Critical(代码质量复核):degenerate pane(`term_px` 窄于一格,如 F4 夹到
    /// 最小尺寸后再被 `grid_size_for` 夹出 1 列)下,`quads_for` 按整格 `cell_w`
    /// 画的底色块必须裁到这个 pane 自己的 `term_px` 内 —— 不裁的话症状是
    /// "邻居 pane 的地盘被画花",复现见评审:pane0 term_px.w=1 但 quad 画出
    /// x=0..10,直接糊到紧邻的 pane1(x=2 起)头上。
    #[test]
    fn degenerate_pane_bg_quad_is_clamped_to_its_own_term_px() {
        let snap = snap_1x1(Rgb::new(205, 0, 0));
        let geom = PaneGeom {
            id: mullion_core::layout::PaneId(1),
            px: PxRect {
                x: 0,
                y: 0,
                w: 1,
                h: 20,
            },
            title_px: PxRect {
                x: 0,
                y: 0,
                w: 1,
                h: 0,
            },
            term_px: PxRect {
                x: 0,
                y: 0,
                w: 1,
                h: 20,
            },
            grid: (1, 1),
        };
        let panes = [PaneRender {
            geom,
            snap: &snap,
            focused: true,
        }];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert_eq!(quads.len(), 1);
        assert_eq!(
            quads[0],
            Quad {
                x: 0.0,
                y: 0.0,
                w: 1.0,
                h: 20.0,
                color: [205, 0, 0]
            },
            "底色块必须裁到 term_px.w=1 内,不能画出整格 cell_w=10"
        );
    }

    /// 光标两种画法都要被裁:Block 整格块和 Hollow 四条边框,degenerate pane 下
    /// 都不能越过自己的 term_px,否则光标会糊到邻居 pane 上。
    #[test]
    fn degenerate_pane_cursor_is_clamped_for_both_styles() {
        let mut a = snap_1x1(Rgb::new(0, 0, 0));
        a.cursor.visible = true;
        let mut b = snap_1x1(Rgb::new(0, 0, 0));
        b.cursor.visible = true;
        let geom = |id: u32, x: u32| PaneGeom {
            id: mullion_core::layout::PaneId(id),
            px: PxRect {
                x,
                y: 0,
                w: 1,
                h: 20,
            },
            title_px: PxRect {
                x,
                y: 0,
                w: 1,
                h: 0,
            },
            term_px: PxRect {
                x,
                y: 0,
                w: 1,
                h: 20,
            },
            grid: (1, 1),
        };
        let panes = [
            PaneRender {
                geom: geom(1, 0),
                snap: &a,
                focused: true,
            }, // Block,term_px = x:0..1
            PaneRender {
                geom: geom(2, 2),
                snap: &b,
                focused: false,
            }, // Hollow,term_px = x:2..3
        ];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert!(!quads.is_empty());
        for q in &quads {
            let (left, right) = if q.x < 2.0 { (0.0, 1.0) } else { (2.0, 3.0) };
            assert!(
                q.x >= left && q.x + q.w <= right,
                "光标 quad 越过自己 pane 的 term_px 边界: {q:?}"
            );
        }
    }

    /// 镜像 `degenerate_pane_bg_quad_is_clamped_to_its_own_term_px`,但退化方向
    /// 是**高度**而非宽度(`term_px.h < cell_h`,宽度给整格)。`MIN_PANE_ROWS = 1`
    /// (`mullion-core/src/layout.rs`)+ 窗口高度不整除时的横向分割,同样会产生
    /// 高度退化的矮 pane —— clamp 实现如果只裁 x 方向、漏了 y,这类布局会画出
    /// 越过 term_px 下边界的色块,复审用「只裁 x」验过原测试套件抓不住这个方向。
    #[test]
    fn degenerate_pane_bg_quad_is_clamped_to_its_own_term_px_in_height() {
        let snap = snap_1x1(Rgb::new(205, 0, 0));
        let geom = PaneGeom {
            id: mullion_core::layout::PaneId(1),
            px: PxRect {
                x: 0,
                y: 0,
                w: 10,
                h: 1,
            },
            title_px: PxRect {
                x: 0,
                y: 0,
                w: 10,
                h: 0,
            },
            term_px: PxRect {
                x: 0,
                y: 0,
                w: 10,
                h: 1,
            },
            grid: (1, 1),
        };
        let panes = [PaneRender {
            geom,
            snap: &snap,
            focused: true,
        }];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert_eq!(quads.len(), 1);
        assert_eq!(
            quads[0],
            Quad {
                x: 0.0,
                y: 0.0,
                w: 10.0,
                h: 1.0,
                color: [205, 0, 0]
            },
            "底色块必须裁到 term_px.h=1 内,不能画出整格 cell_h=20"
        );
    }

    /// 镜像 `degenerate_pane_cursor_is_clamped_for_both_styles`,退化方向是高度:
    /// 两个 pane 上下相邻而非左右相邻,Block/Hollow 光标都不能越过自己的
    /// term_px 下边界。
    #[test]
    fn degenerate_pane_cursor_is_clamped_for_both_styles_in_height() {
        let mut a = snap_1x1(Rgb::new(0, 0, 0));
        a.cursor.visible = true;
        let mut b = snap_1x1(Rgb::new(0, 0, 0));
        b.cursor.visible = true;
        let geom = |id: u32, y: u32| PaneGeom {
            id: mullion_core::layout::PaneId(id),
            px: PxRect {
                x: 0,
                y,
                w: 10,
                h: 1,
            },
            title_px: PxRect {
                x: 0,
                y,
                w: 10,
                h: 0,
            },
            term_px: PxRect {
                x: 0,
                y,
                w: 10,
                h: 1,
            },
            grid: (1, 1),
        };
        let panes = [
            PaneRender {
                geom: geom(1, 0),
                snap: &a,
                focused: true,
            }, // Block,term_px = y:0..1
            PaneRender {
                geom: geom(2, 2),
                snap: &b,
                focused: false,
            }, // Hollow,term_px = y:2..3
        ];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert!(!quads.is_empty());
        for q in &quads {
            let (top, bottom) = if q.y < 2.0 { (0.0, 1.0) } else { (2.0, 3.0) };
            assert!(
                q.y >= top && q.y + q.h <= bottom,
                "光标 quad 越过自己 pane 的 term_px 边界(y 方向): {q:?}"
            );
        }
    }

    /// bg quad 整个落在 term_px 外时必须被**整体丢弃**,不能留下一个 w<=0 的
    /// 退化 quad。之前的用例(width/height 退化)裁完宽/高都还 > 0,从没真正
    /// 走到 `clamp_quad_to_bounds` 返回 `None` 的分支——那条分支此前只被光标
    /// 测试顺带盖到,没有独立锁定 bg quad 的丢弃路径。
    /// `term_px.w = 0` 是最直接的复现:col 0 的 quad 起点就是 term_px 左边界,
    /// 裁剪后右边界与左边界重合,宽度精确为 0。
    #[test]
    fn bg_quad_entirely_outside_term_px_is_dropped_not_shrunk_to_zero() {
        let snap = snap_1x1(Rgb::new(205, 0, 0));
        let geom = PaneGeom {
            id: mullion_core::layout::PaneId(1),
            px: PxRect {
                x: 5,
                y: 0,
                w: 0,
                h: 20,
            },
            title_px: PxRect {
                x: 5,
                y: 0,
                w: 0,
                h: 0,
            },
            term_px: PxRect {
                x: 5,
                y: 0,
                w: 0,
                h: 20,
            },
            grid: (1, 1),
        };
        let panes = [PaneRender {
            geom,
            snap: &snap,
            focused: true,
        }];
        let quads = quads_for_panes(&panes, 10.0, 20.0, DefaultColors::default());
        assert!(
            quads.is_empty(),
            "term_px.w=0 时 bg quad 应被整体丢弃(clamp 后 w<=0),不应留下退化 quad: {quads:?}"
        );
    }
}
