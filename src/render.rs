//! Draws a clip's frames on the GPU, the same on every platform: the recording placed by the
//! view and zoom inside the rounded frame, over the background gradient and the frame's shadow
//! (render.wgsl). The export encodes its output and the review will show it, so what's
//! previewed is what's exported. It owns its own wgpu device (wgpu 30, as Rune's): Metal on
//! macOS, DX12 on Windows, Vulkan on Linux.

use crate::export::Canvas;
use crate::settings::Background;

/// One frame's scene, in pixels.
#[derive(Clone, Copy, Debug)]
pub struct Scene {
    pub canvas: Canvas,
    /// The recording's pixels the frame shows (x, y, w, h).
    pub shown: [f64; 4],
    /// The frame's corner radius (`export::frame_radius`).
    pub radius: f64,
    pub background: Background,
}

/// The order of a frame's four bytes a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pixels {
    Rgba,
    Bgra,
}

impl Pixels {
    fn format(self) -> wgpu::TextureFormat {
        match self { Pixels::Rgba => wgpu::TextureFormat::Rgba8Unorm, Pixels::Bgra => wgpu::TextureFormat::Bgra8Unorm }
    }
}

/// A frame of the recording: `h` rows of `stride` bytes, `w` pixels of `pixels` each.
pub struct Image<'a> {
    pub w: u32,
    pub h: u32,
    pub stride: u32,
    pub pixels: Pixels,
    pub data: &'a [u8],
}

/// The frame's shadow: opacity, blur as a fraction of the frame's long side, and how far
/// down it falls as a fraction of its height (as Core Animation drew it).
const SHADOW_OPACITY: f32 = 0.45;
const SHADOW_BLUR: f64 = 0.025;
const SHADOW_DROP: f64 = 0.012;
/// Most samples per side when downscaling.
const MAX_TAPS: f64 = 4.0;

pub struct Renderer {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniforms: wgpu::Buffer,
    /// The recording's texture, kept while its size and format stay the same.
    source: Option<(wgpu::Texture, u32, u32, Pixels)>,
    /// The canvas's byte order.
    output: Pixels,
    /// The canvas texture and the buffer it's read back through, kept per canvas size.
    target: Option<(wgpu::Texture, wgpu::Buffer, u32, u32)>,
}

/// Rows of a texture copy are padded to this many bytes.
fn padded_row(w: u32) -> u32 {
    (w * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT
}

fn rgb(c: u32) -> [f32; 4] {
    [((c >> 16) & 0xff) as f32 / 255.0, ((c >> 8) & 0xff) as f32 / 255.0, (c & 0xff) as f32 / 255.0, 1.0]
}

impl Renderer {
    /// A renderer whose canvases come back in `output` byte order.
    pub fn new(output: Pixels) -> Result<Renderer, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        })).map_err(|e| format!("no GPU for rendering ({e})"))?;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("clip-render"),
            ..Default::default()
        })).map_err(|e| format!("couldn't open the GPU ({e})"))?;
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("clip-render"),
            source: wgpu::ShaderSource::Wgsl(include_str!("render.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("clip-render"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("clip-render"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("clip-render"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs"), compilation_options: Default::default(), buffers: &[] },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: output.format(), blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("clip-render"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("clip-render"),
            size: 7 * 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Renderer { device, queue, pipeline, layout, sampler, uniforms, source: None, output, target: None })
    }

    /// Uploads `frame` as the recording the next renders draw.
    pub fn upload(&mut self, frame: &Image) -> Result<(), String> {
        if frame.stride < frame.w * 4 || frame.data.len() < (frame.stride * (frame.h - 1) + frame.w * 4) as usize {
            return Err("the frame is smaller than its size".into());
        }
        if self.source.as_ref().is_none_or(|s| (s.1, s.2, s.3) != (frame.w, frame.h, frame.pixels)) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("clip-recording"),
                size: wgpu::Extent3d { width: frame.w, height: frame.h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: frame.pixels.format(),
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            self.source = Some((texture, frame.w, frame.h, frame.pixels));
        }
        let (texture, ..) = self.source.as_ref().expect("made above");
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            frame.data,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(frame.stride), rows_per_image: Some(frame.h) },
            wgpu::Extent3d { width: frame.w, height: frame.h, depth_or_array_layers: 1 },
        );
        Ok(())
    }

    /// The size of the last uploaded frame.
    pub fn source_size(&self) -> Option<(u32, u32)> {
        self.source.as_ref().map(|s| (s.1, s.2))
    }

    /// Draws `scene` over the last uploaded frame, and returns the canvas in the output byte
    /// order (rows `canvas.w * 4` bytes).
    pub fn render(&mut self, scene: &Scene) -> Result<Vec<u8>, String> {
        let (cw, ch) = (scene.canvas.w, scene.canvas.h);
        let Some((_, sw, sh, _)) = self.source.as_ref() else { return Err("no frame to draw".into()) };
        let (sw, sh) = (*sw, *sh);
        if self.target.as_ref().is_none_or(|t| (t.2, t.3) != (cw, ch)) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("clip-canvas"),
                size: wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.output.format(),
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("clip-readback"),
                size: (padded_row(cw) * ch) as u64,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            });
            self.target = Some((texture, buffer, cw, ch));
        }
        let (source, ..) = self.source.as_ref().expect("checked above");
        let (target, readback, _, _) = self.target.as_ref().expect("made above");

        let c = &scene.canvas;
        let k = (c.inner[2] / scene.shown[2]).min(c.inner[3] / scene.shown[3]);
        let taps = (1.0 / k).ceil().clamp(1.0, MAX_TAPS);
        let (a, b) = scene.background.colors();
        let bg = scene.background != Background::None;
        let long = c.inner[2].max(c.inner[3]);
        let values: [[f32; 4]; 7] = [
            [cw as f32, ch as f32, sw as f32, sh as f32],
            c.inner.map(|v| v as f32),
            scene.shown.map(|v| v as f32),
            [scene.radius as f32, if bg { 1.0 } else { 0.0 }, taps as f32, 0.0],
            rgb(a),
            rgb(b),
            [SHADOW_OPACITY, (long * SHADOW_BLUR / 2.0) as f32, (c.inner[3] * SHADOW_DROP) as f32, 0.0],
        ];
        let bytes: Vec<u8> = values.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        self.queue.write_buffer(&self.uniforms, 0, &bytes);

        let view = source.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("clip-render"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.uniforms.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        let out = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("clip-render") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clip-render"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &out,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        let row = padded_row(cw);
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: target, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(ch) } },
            wgpu::Extent3d { width: cw, height: ch, depth_or_array_layers: 1 },
        );
        self.queue.submit([encoder.finish()]);

        let slice = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| { let _ = tx.send(r); });
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).map_err(|e| format!("the GPU didn't finish ({e})"))?;
        rx.recv().map_err(|_| "the GPU dropped the frame")?.map_err(|e| format!("couldn't read the frame back ({e})"))?;
        let mut rgba = Vec::with_capacity((cw * ch * 4) as usize);
        {
            let data = slice.get_mapped_range().map_err(|e| format!("couldn't read the frame back ({e:?})"))?;
            for y in 0..ch as usize {
                let start = y * row as usize;
                rgba.extend_from_slice(&data[start..start + cw as usize * 4]);
            }
        }
        readback.unmap();
        Ok(rgba)
    }
}

/// `clip --render-frame <recording.mov> <seconds> <out.png> [background]`: renders one frame of
/// a recording the way the review would export it (its clicks zoomed, Desktop shot, whole
/// trim, original size), for checking the renderer against the export. Exits.
pub fn frame_cli(args: &[String]) -> ! {
    let run = || -> Result<(), String> {
        let [raw, t, out, rest @ ..] = args else { return Err("usage: --render-frame <recording.mov> <seconds> <out.png> [None|Dusk|Ocean|Mint|Graphite]".into()) };
        let raw = std::path::Path::new(raw);
        let t: f64 = t.parse().map_err(|_| "seconds must be a number")?;
        let background = match rest.first().map(String::as_str) {
            Some("Dusk") => Background::Dusk, Some("Ocean") => Background::Ocean, Some("Mint") => Background::Mint,
            Some("Graphite") => Background::Graphite, _ => Background::None,
        };
        let track = crate::track::Track::load(raw);
        let frame = crate::thumbs::frame_at(raw, t).ok_or("couldn't decode that frame")?;
        let duration = crate::thumbs::duration(raw).ok_or("couldn't read the movie")?;
        let px = (frame.w as f64, frame.h as f64);
        let (views, size) = crate::view::plan(crate::view::Source::Desktop, &track.windows, [0.0, 0.0, 1.0, 1.0], px, 0.0, duration);
        let zoom = crate::zoom::plan(&track.clicks, |t| crate::view::sample(&views, t), 0.0, duration);
        let shown = crate::view::shown(crate::view::sample(&views, t), crate::zoom::sample(&zoom, t));
        let shown = [shown[0] * px.0, shown[1] * px.1, shown[2] * px.0, shown[3] * px.1];
        let canvas = crate::export::canvas(size.0, size.1, background, None, track.corner);
        let radius = crate::export::frame_radius(&canvas, track.corner, (frame.w, frame.h), shown);
        let mut renderer = Renderer::new(Pixels::Rgba)?;
        let started = std::time::Instant::now();
        renderer.upload(&Image { w: frame.w, h: frame.h, stride: frame.w * 4, pixels: Pixels::Rgba, data: &frame.rgba })?;
        let rgba = renderer.render(&Scene { canvas, shown, radius, background })?;
        eprintln!("rendered {}x{} in {:.1} ms (first frame, with setup)", canvas.w, canvas.h, started.elapsed().as_secs_f64() * 1000.0);
        let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), canvas.w, canvas.h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().and_then(|mut w| w.write_image_data(&rgba)).map_err(|e| e.to_string())?;
        Ok(())
    };
    match run() {
        Ok(()) => std::process::exit(0),
        Err(e) => { eprintln!("clip: {e}"); std::process::exit(1) }
    }
}
