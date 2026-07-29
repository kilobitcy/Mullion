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
pub fn quads_for(
    snap: &GridSnapshot,
    origin: (f32, f32),
    cell_w: f32,
    cell_h: f32,
    defaults: DefaultColors,
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
        quads.push(Quad {
            x: origin.0 + snap.cursor.col as f32 * cell_w,
            y: origin.1 + snap.cursor.row as f32 * cell_h,
            w: cell_w,
            h: cell_h,
            // MVP 块状光标用默认前景色。原本硬编码 0xcc,主题化后必须跟着走,
            // 否则新前景下光标是一块突兀的旧灰。
            color: [defaults.fg.r, defaults.fg.g, defaults.fg.b],
        });
    }
    quads
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
    use mullion_term::palette::DefaultColors;
    use mullion_term::snapshot::{Cursor, Rgb, SnapCell};

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
            },
        }
    }

    #[test]
    fn origin_shifts_every_quad_so_first_row_clears_the_menu_bar() {
        // 真机症状:第 0 行画在窗口顶端(y=0),被 egui 顶部菜单栏整条盖住,
        // 用户看不到登录横幅第一行。行数已按中央区算过,漏的是这一下平移。
        let mut snap = snap_1x1(Rgb::new(205, 0, 0));
        snap.cursor.visible = true;
        let quads = quads_for(&snap, (0.0, 24.0), 10.0, 20.0, DefaultColors::default());
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
        let quads = quads_for(&snap, (0.0, 0.0), 10.0, 20.0, DefaultColors::default());
        assert!(quads.is_empty(), "默认背景不该产生色块(省 GPU)");
    }

    #[test]
    fn colored_bg_cell_makes_quad_at_pixel() {
        let snap = snap_1x1(Rgb::new(205, 0, 0));
        let quads = quads_for(&snap, (0.0, 0.0), 10.0, 20.0, DefaultColors::default());
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
        let quads = quads_for(&snap, (0.0, 0.0), 10.0, 20.0, DefaultColors::default());
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
            },
        }
    }

    #[test]
    fn selected_cell_is_inverted_even_on_default_background() {
        // 反色必须优先于「bg 是默认色就不画」这条既有短路,否则在默认背景上
        // (也就是绝大多数情况)选区完全看不见。
        let fg = Rgb::new(0xcc, 0xcc, 0xcc);
        let snap = snap_selected_1x1(fg, Rgb::new(0, 0, 0));
        let quads = quads_for(&snap, (0.0, 0.0), 10.0, 20.0, DefaultColors::default());
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
        };
        let quads = quads_for(&snap, (0.0, 0.0), 10.0, 20.0, DefaultColors { fg, bg });
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
        );
        assert!(
            quads.is_empty(),
            "背景 == 主题默认背景 的格子不该画 quad,否则白扔一块画面"
        );
    }
}
