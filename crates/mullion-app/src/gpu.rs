//! GPU 层:背景/光标色块生成(纯,可测)+ wgpu 表面与色块管线(GPU 胶水,见 Task 8)。

use mullion_term::snapshot::{GridSnapshot, Rgb};

/// 一个实心色块(背景 / 光标),像素坐标(左上原点)。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub color: [u8; 3],
}

/// 从快照生成需要画的色块:bg ≠ 默认 的格 + 可见光标(块状)。纯函数,可单测。
pub fn quads_for(snap: &GridSnapshot, cell_w: f32, cell_h: f32, default_bg: Rgb) -> Vec<Quad> {
    let mut quads = Vec::new();
    for row in 0..snap.rows {
        for (col, cell) in snap.row(row).iter().enumerate() {
            if cell.spacer || cell.bg == default_bg {
                continue;
            }
            quads.push(Quad {
                x: col as f32 * cell_w,
                y: row as f32 * cell_h,
                w: cell.width.max(1) as f32 * cell_w,
                h: cell_h,
                color: [cell.bg.r, cell.bg.g, cell.bg.b],
            });
        }
    }
    if snap.cursor.visible {
        quads.push(Quad {
            x: snap.cursor.col as f32 * cell_w,
            y: snap.cursor.row as f32 * cell_h,
            w: cell_w,
            h: cell_h,
            color: [0xcc, 0xcc, 0xcc], // MVP 块状光标用默认前景色
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
        let (device, queue) = handle
            .block_on(adapter.request_device(&wgpu::DeviceDescriptor::default(), None))
            .expect("request_device");

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
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> { return in.color; }
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use mullion_term::snapshot::{Cursor, SnapCell};

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
            }],
            cursor: Cursor {
                row: 0,
                col: 0,
                visible: false,
            },
        }
    }

    #[test]
    fn default_bg_cell_makes_no_quad() {
        let snap = snap_1x1(Rgb::new(0, 0, 0)); // == DEFAULT_BG
        let quads = quads_for(&snap, 10.0, 20.0, Rgb::new(0, 0, 0));
        assert!(quads.is_empty(), "默认背景不该产生色块(省 GPU)");
    }

    #[test]
    fn colored_bg_cell_makes_quad_at_pixel() {
        let snap = snap_1x1(Rgb::new(205, 0, 0));
        let quads = quads_for(&snap, 10.0, 20.0, Rgb::new(0, 0, 0));
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
        let quads = quads_for(&snap, 10.0, 20.0, Rgb::new(0, 0, 0));
        assert_eq!(quads.len(), 1, "仅光标块(默认背景无块)");
        assert_eq!(quads[0].w, 10.0);
    }
}
