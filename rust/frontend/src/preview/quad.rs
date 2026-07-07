// preview/quad.rs — textured-quad pipeline shared by every bitmap-bearing
// preview renderer (png, svg-via-resvg, future video frames, …).
//
// Renders an RGBA8 texture into a screen-space rect specified in physical
// pixels. The pipeline takes (rect, surface_size) → clip-space quad in the
// vertex shader; the fragment shader samples the texture. No transforms, no
// blending math — just a textured quad with alpha blending.

use anyhow::Result;
use wgpu::util::DeviceExt;

#[repr(C)]
#[derive(Clone, Copy, Debug, bytemuck_derive::Pod, bytemuck_derive::Zeroable)]
struct Vertex {
    position: [f32; 2], // clip-space [-1, 1]
    uv: [f32; 2],
}

const SHADER: &str = r#"
struct VsIn {
    @location(0) pos: vec2<f32>,
    @location(1) uv:  vec2<f32>,
};
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_main(in: VsIn) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(in.pos, 0.0, 1.0);
    out.uv   = in.uv;
    return out;
}

@group(0) @binding(0) var t_tex: texture_2d<f32>;
@group(0) @binding(1) var s_tex: sampler;

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(t_tex, s_tex, in.uv);
}
"#;

pub struct QuadPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    /// Default linear-filtered sampler — used by math SVGs, markdown
    /// figures, and the chrome bg/border quads where smoothing on
    /// resample reads more naturally.
    sampler: wgpu::Sampler,
    /// Nearest-neighbour sampler for the standalone PNG preview path.
    /// Lets the user see individual data pixels when zooming a heatmap
    /// or similar scientific image without bilinear blur smearing
    /// neighbouring cells together (user ask 2026-05-22).
    sampler_nearest: wgpu::Sampler,
}

impl QuadPipeline {
    pub fn new(device: &wgpu::Device, surface_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("quad-shader"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });

        let bind_group_layout =
            device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("quad-bgl"),
                entries: &[
                    wgpu::BindGroupLayoutEntry {
                        binding: 0,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Texture {
                            sample_type: wgpu::TextureSampleType::Float { filterable: true },
                            view_dimension: wgpu::TextureViewDimension::D2,
                            multisampled: false,
                        },
                        count: None,
                    },
                    wgpu::BindGroupLayoutEntry {
                        binding: 1,
                        visibility: wgpu::ShaderStages::FRAGMENT,
                        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                        count: None,
                    },
                ],
            });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("quad-pl"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("quad-pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: Default::default(),
                buffers: &[wgpu::VertexBufferLayout {
                    array_stride: std::mem::size_of::<Vertex>() as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2],
                }],
            },
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                cull_mode: None,
                ..Default::default()
            },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: surface_format,
                    blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("quad-sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });
        let sampler_nearest = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("quad-sampler-nearest"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            mipmap_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
            sampler_nearest,
        }
    }
}

/// Rectangle in physical-pixel coordinates; origin top-left.
#[derive(Clone, Copy, Debug)]
pub struct ScreenRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One uploaded RGBA8 texture, ready to draw into any rect.
pub struct Quad {
    bind_group: wgpu::BindGroup,
    /// Vertex buffer holding the current rect's 6 verts. We rewrite it each
    /// frame (cheap, 96 bytes) so callers can move the rect freely.
    vbuf: wgpu::Buffer,
    pub size_px: (u32, u32),
}

/// Which sampler to bind when building a Quad. `Linear` is the default
/// (smoother resample, right for photographs / vector-rendered glyphs);
/// `Nearest` preserves individual source pixels (right for scientific
/// raster previews where each cell carries meaning).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SamplerKind {
    Linear,
    Nearest,
}

impl Quad {
    pub fn from_rgba8(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &QuadPipeline,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Self> {
        Self::from_rgba8_with_sampler(
            device, queue, pipeline, rgba, width, height, SamplerKind::Linear,
        )
    }

    pub fn from_rgba8_with_sampler(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &QuadPipeline,
        rgba: &[u8],
        width: u32,
        height: u32,
        sampler: SamplerKind,
    ) -> Result<Self> {
        if (rgba.len() as u32) < width * height * 4 {
            anyhow::bail!(
                "rgba buffer too small: {} bytes for {}x{} (need {})",
                rgba.len(),
                width,
                height,
                width * height * 4
            );
        }

        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quad-texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8UnormSrgb,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::ImageCopyTexture {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &rgba[..(width * height * 4) as usize],
            wgpu::ImageDataLayout {
                offset: 0,
                bytes_per_row: Some(4 * width),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );

        let sampler_ref = match sampler {
            SamplerKind::Linear => &pipeline.sampler,
            SamplerKind::Nearest => &pipeline.sampler_nearest,
        };
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("quad-bg"),
            layout: &pipeline.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(sampler_ref),
                },
            ],
        });

        // Placeholder vbuf; rewritten on each render.
        let vbuf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("quad-vbuf"),
            contents: bytemuck::cast_slice(&[Vertex {
                position: [0.0, 0.0],
                uv: [0.0, 0.0],
            }; 6]),
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        });

        Ok(Self {
            bind_group,
            vbuf,
            size_px: (width, height),
        })
    }

    pub fn render<'a>(
        &'a self,
        queue: &wgpu::Queue,
        pipeline: &'a QuadPipeline,
        render_pass: &mut wgpu::RenderPass<'a>,
        rect: ScreenRect,
        surface_size: (u32, u32),
    ) -> Result<()> {
        let verts = rect_verts(rect, surface_size);
        queue.write_buffer(&self.vbuf, 0, bytemuck::cast_slice(&verts));

        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.vbuf.slice(..));
        render_pass.draw(0..6, 0..1);
        Ok(())
    }

    /// Batched render for multi-rect callers (markdown code-bg, strike
    /// line, multi-row text selection, …). Each call to `render` rewrites
    /// `vbuf` at offset 0, so calling it in a per-rect loop within one
    /// render pass clobbers every previous write — only the last rect's
    /// vertices reach the GPU. This path uploads N*6 verts in one
    /// `queue.write_buffer`, grows `vbuf` if needed, and issues a single
    /// draw. Empty input is a no-op.
    pub fn render_many<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &'a QuadPipeline,
        render_pass: &mut wgpu::RenderPass<'a>,
        rects: &[ScreenRect],
        surface_size: (u32, u32),
    ) -> Result<()> {
        if rects.is_empty() {
            return Ok(());
        }
        let mut verts: Vec<Vertex> = Vec::with_capacity(rects.len() * 6);
        for r in rects {
            verts.extend_from_slice(&rect_verts(*r, surface_size));
        }
        let bytes = bytemuck::cast_slice::<Vertex, u8>(&verts);
        let needed = bytes.len() as wgpu::BufferAddress;
        if self.vbuf.size() < needed {
            // Grow with a little headroom so back-to-back resizes are
            // rare when one frame has 30 rects and the next has 40.
            let new_size = (needed.next_power_of_two()).max(96);
            self.vbuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quad-vbuf-batched"),
                size: new_size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.vbuf, 0, bytes);

        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.vbuf.slice(..));
        render_pass.draw(0..(verts.len() as u32), 0..1);
        Ok(())
    }

    /// Like [`render_many`], but each rect's quad is spun about its own center
    /// by `angle` radians before projection (a positive angle spins clockwise
    /// on the y-down screen). The texture rides along — used for the brand
    /// wheels in the bottom session strip, which flick when the user cycles
    /// workspaces. Rotation is applied in pixel space so a square logo stays
    /// circular regardless of the surface aspect ratio.
    pub fn render_many_rotated<'a>(
        &'a mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        pipeline: &'a QuadPipeline,
        render_pass: &mut wgpu::RenderPass<'a>,
        rects: &[ScreenRect],
        angle: f32,
        surface_size: (u32, u32),
    ) -> Result<()> {
        if rects.is_empty() {
            return Ok(());
        }
        let mut verts: Vec<Vertex> = Vec::with_capacity(rects.len() * 6);
        for r in rects {
            verts.extend_from_slice(&rect_verts_rotated(*r, surface_size, angle));
        }
        let bytes = bytemuck::cast_slice::<Vertex, u8>(&verts);
        let needed = bytes.len() as wgpu::BufferAddress;
        if self.vbuf.size() < needed {
            let new_size = (needed.next_power_of_two()).max(96);
            self.vbuf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quad-vbuf-batched"),
                size: new_size,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.vbuf, 0, bytes);

        render_pass.set_pipeline(&pipeline.pipeline);
        render_pass.set_bind_group(0, &self.bind_group, &[]);
        render_pass.set_vertex_buffer(0, self.vbuf.slice(..));
        render_pass.draw(0..(verts.len() as u32), 0..1);
        Ok(())
    }
}

fn rect_verts(rect: ScreenRect, surface_size: (u32, u32)) -> [Vertex; 6] {
    let (sw, sh) = (surface_size.0.max(1) as f32, surface_size.1.max(1) as f32);
    let to_clip_x = |px: f32| (px / sw) * 2.0 - 1.0;
    // y inverted: top-left in pixels = +1 in clip
    let to_clip_y = |px: f32| 1.0 - (px / sh) * 2.0;
    let x0 = to_clip_x(rect.x);
    let x1 = to_clip_x(rect.x + rect.w);
    let y0 = to_clip_y(rect.y);
    let y1 = to_clip_y(rect.y + rect.h);
    [
        Vertex { position: [x0, y0], uv: [0.0, 0.0] },
        Vertex { position: [x1, y0], uv: [1.0, 0.0] },
        Vertex { position: [x1, y1], uv: [1.0, 1.0] },
        Vertex { position: [x0, y0], uv: [0.0, 0.0] },
        Vertex { position: [x1, y1], uv: [1.0, 1.0] },
        Vertex { position: [x0, y1], uv: [0.0, 1.0] },
    ]
}

/// Same triangle winding + UVs as [`rect_verts`], but each corner is rotated
/// about the rect's pixel-space center by `angle` before the clip projection.
/// Done in pixels (not clip space) so the wheel stays circular whatever the
/// surface aspect ratio; a positive angle reads as clockwise on the y-down
/// screen.
fn rect_verts_rotated(rect: ScreenRect, surface_size: (u32, u32), angle: f32) -> [Vertex; 6] {
    let (sw, sh) = (surface_size.0.max(1) as f32, surface_size.1.max(1) as f32);
    let to_clip_x = |px: f32| (px / sw) * 2.0 - 1.0;
    let to_clip_y = |px: f32| 1.0 - (px / sh) * 2.0;
    let cx = rect.x + rect.w / 2.0;
    let cy = rect.y + rect.h / 2.0;
    let (s, c) = angle.sin_cos();
    let rot = |px: f32, py: f32| -> [f32; 2] {
        let dx = px - cx;
        let dy = py - cy;
        [to_clip_x(dx * c - dy * s + cx), to_clip_y(dx * s + dy * c + cy)]
    };
    let p00 = rot(rect.x, rect.y);
    let p10 = rot(rect.x + rect.w, rect.y);
    let p11 = rot(rect.x + rect.w, rect.y + rect.h);
    let p01 = rot(rect.x, rect.y + rect.h);
    [
        Vertex { position: p00, uv: [0.0, 0.0] },
        Vertex { position: p10, uv: [1.0, 0.0] },
        Vertex { position: p11, uv: [1.0, 1.0] },
        Vertex { position: p00, uv: [0.0, 0.0] },
        Vertex { position: p11, uv: [1.0, 1.0] },
        Vertex { position: p01, uv: [0.0, 1.0] },
    ]
}

// `Quad` does not borrow from `QuadPipeline` for its bind_group — the bind
// group depends on the pipeline's BindGroupLayout but only the layout is
// captured during creation, not a reference. That's important: callers can
// hold the pipeline once on State and the Quads independently, without
// borrow-checker conflicts.
fn _assert_quad_send_sync()
where
    Quad: Send + Sync,
{
}
