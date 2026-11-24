//! GPU renderer: every rectangle is one instanced quad. The shader draws a 1 px outline, a light fill with a
//! hatch for large shapes, and a solid fill for thin ones; tiny shapes are kept at one pixel.
use crate::def::Layout;
use crate::scene::{overview_style_base, Inst, Scene, Tiles};
use bytemuck::{Pod, Zeroable};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use eframe::wgpu;
use eframe::wgpu::util::DeviceExt;

pub const SHADER: &str = r#"
struct Cam { origin: vec2<i32>, size: vec2<f32>, scale: f32, dpr: f32, pad0: f32, pad1: f32 };
struct Style { color: vec4<f32>, p: vec4<f32> };
@group(0) @binding(0) var<uniform> cam: Cam;
@group(0) @binding(1) var<uniform> styles: array<Style, 512>;
struct VOut {
  @builtin(position) pos: vec4<f32>,
  @location(0) local: vec2<f32>,
  @location(1) sizepx: vec2<f32>,
  @location(2) @interpolate(flat) slot: u32,
};
@vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) r: vec4<i32>, @location(1) slot: u32) -> VOut {
  let corner = vec2<f32>(f32(vi & 1u), f32((vi >> 1u) & 1u));
  let a = vec2<f32>(r.xy - cam.origin) * cam.scale;
  let b = vec2<f32>(r.zw - cam.origin) * cam.scale;
  let szpx = max(b - a, vec2<f32>(1.0, 1.0));
  let c = (a + b) * 0.5;
  let p = c + (corner - vec2<f32>(0.5, 0.5)) * szpx;
  var o: VOut;
  o.pos = vec4<f32>(p.x / (cam.size.x * 0.5), p.y / (cam.size.y * 0.5), 0.0, 1.0);
  o.local = corner * szpx;
  o.sizepx = szpx;
  o.slot = slot;
  return o;
}
@fragment fn fs(v: VOut) -> @location(0) vec4<f32> {
  let st = styles[v.slot];
  let flags = u32(st.p.z);
  let w = v.sizepx;
  let l = v.local;
  let edge = l.x < 1.0 || l.y < 1.0 || l.x > w.x - 1.0 || l.y > w.y - 1.0;
  var a = st.p.x;
  if ((flags & 4u) != 0u) {
    a = st.p.x;
  } else if (edge) {
    a = 1.0;
  } else if ((flags & 2u) != 0u) {
    discard;
  } else if (min(w.x, w.y) < 6.0) {
    a = 0.8;
  } else if ((flags & 1u) != 0u) {
    a = 0.6;
  } else {
    let h = u32(st.p.y);
    let fx = i32(floor(v.pos.x));
    let fy = i32(floor(v.pos.y));
    if (h == 1u && ((fx + fy) % 6 + 6) % 6 == 0) { a = 0.85; }
    if (h == 2u && ((fx - fy) % 6 + 6) % 6 == 0) { a = 0.85; }
  }
  return vec4<f32>(st.color.rgb * a, a);
}
"#;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct Cam {
    pub origin: [i32; 2],
    pub size: [f32; 2],
    pub scale: f32,
    pub dpr: f32,
    pub pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct Style {
    pub color: [f32; 4],
    /// fill alpha, hatch (0, 1, 2), flags (1 cut, 2 outline only, 4 plain fill), unused
    pub p: [f32; 4],
}

/// style table size: a uniform array (OpenGL allows no storage buffer in the fragment stage); 512 x 32 B = 16 KB,
/// the smallest uniform block every backend guarantees
pub const MAX_STYLES: usize = 512;
static STYLE_OVERFLOW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
const CHUNK: usize = 6 << 20; // instances per vertex buffer (120 MB)

pub struct Gpu {
    pub pipeline: wgpu::RenderPipeline,
    pub bind_group: wgpu::BindGroup,
    pub cam_buf: wgpu::Buffer,
    pub style_buf: wgpu::Buffer,
    pub chunks: Vec<(wgpu::Buffer, u32)>,
    pub slot_range: Vec<(u32, u32)>,
    pub extra: Option<(wgpu::Buffer, u32)>,
    pub hl: Option<(wgpu::Buffer, u32)>,
    pub hl_version: u64,
    /// large layouts: instance ranges by tile (draw only the view) and the zoomed-out overview
    pub tiles: Option<Tiles>,
    pub ov: Vec<(wgpu::Buffer, u32)>,
    pub ov_range: Vec<(u32, u32)>,
    pub ov_bin: f64,
    /// instances drawn by the last frame, and whether it drew the overview (for the LSV_DRAW_STATS test hook)
    pub drawn: AtomicU64,
    pub drew_overview: AtomicBool,
}

/// overview used when one bin is at most this many pixels
const OV_MAX_PX: f64 = 1.5;

/// second renderer instance for the schematic overview (its own buffers next to the layout's)
pub struct SchGpu(pub Gpu);

/// what to draw in one frame
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cam: Cam,
    pub styles: Vec<Style>,
    pub visible: Vec<bool>,
    /// instances drawn under the highlight: die outline, then the fading veil
    pub extra: Vec<Inst>,
    /// index in `extra` from which instances are drawn after the regular shapes
    pub veil_from: usize,
    pub hl: std::sync::Arc<Vec<Inst>>,
    pub hl_version: u64,
}

impl Gpu {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Gpu {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("lsv"), source: wgpu::ShaderSource::Wgsl(SHADER.into()) });
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lsv"),
            entries: &[
                wgpu::BindGroupLayoutEntry { binding: 0, visibility: wgpu::ShaderStages::VERTEX, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }, count: None },
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("lsv"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let attrs = [wgpu::VertexAttribute { format: wgpu::VertexFormat::Sint32x4, offset: 0, shader_location: 0 }, wgpu::VertexAttribute { format: wgpu::VertexFormat::Uint32, offset: 16, shader_location: 1 }];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lsv"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Inst>() as u64, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs })],
            },
            primitive: wgpu::PrimitiveState { topology: wgpu::PrimitiveTopology::TriangleStrip, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING), write_mask: wgpu::ColorWrites::ALL })],
            }),
            multiview_mask: None,
            cache: None,
        });
        let cam_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("cam"), size: std::mem::size_of::<Cam>() as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let style_buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("styles"), size: (MAX_STYLES * std::mem::size_of::<Style>()) as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lsv"),
            layout: &bgl,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: cam_buf.as_entire_binding() }, wgpu::BindGroupEntry { binding: 1, resource: style_buf.as_entire_binding() }],
        });
        Gpu { pipeline, bind_group, cam_buf, style_buf, chunks: vec![], slot_range: vec![], extra: None, hl: None, hl_version: u64::MAX, tiles: None, ov: vec![], ov_range: vec![], ov_bin: 0.0, drawn: AtomicU64::new(0), drew_overview: AtomicBool::new(false) }
    }

    pub fn upload(&mut self, device: &wgpu::Device, scene: &Scene) {
        self.upload_raw(device, &scene.inst, scene.slot_range.clone());
    }

    /// upload a scene made by Scene::build_lean: each chunk of instances is made from the layout, sent and freed,
    /// so the whole instance array never exists in main memory
    pub fn upload_layout(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, scene: &Scene, lay: &Layout) {
        if !scene.inst.is_empty() || scene.order.is_empty() && lay.comps.is_empty() {
            return self.upload(device, scene);
        }
        self.chunks.clear();
        let total = scene.inst_count();
        let mut a = 0;
        while a < total {
            let b = (a + CHUNK).min(total);
            let part = scene.lean_insts(lay, a, b);
            let size = std::mem::size_of_val(&part[..]) as u64;
            let buf = device.create_buffer(&wgpu::BufferDescriptor { label: Some("inst"), size, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
            queue.write_buffer(&buf, 0, bytemuck::cast_slice(&part));
            // send this chunk now, so its staging copy is freed before the next chunk is made
            queue.submit(std::iter::empty());
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            self.chunks.push((buf, part.len() as u32));
            a = b;
        }
        self.slot_range = scene.slot_range.clone();
        self.tiles = scene.tiles.clone();
        if let Some(o) = &scene.overview {
            for part in o.inst.chunks(CHUNK) {
                let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("overview"), contents: bytemuck::cast_slice(part), usage: wgpu::BufferUsages::VERTEX });
                self.ov.push((buf, part.len() as u32));
            }
            self.ov_range = o.slot_range.clone();
            self.ov_bin = o.bin;
        }
    }

    /// instances sorted by slot, with the [start, end) range of every slot
    pub fn upload_raw(&mut self, device: &wgpu::Device, inst: &[Inst], slot_range: Vec<(u32, u32)>) {
        self.chunks.clear();
        self.tiles = None;
        self.ov.clear();
        self.ov_range.clear();
        for part in inst.chunks(CHUNK) {
            let buf = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("inst"), contents: bytemuck::cast_slice(part), usage: wgpu::BufferUsages::VERTEX });
            self.chunks.push((buf, part.len() as u32));
        }
        self.slot_range = slot_range;
    }

    pub fn prepare(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, f: &Frame) {
        queue.write_buffer(&self.cam_buf, 0, bytemuck::bytes_of(&f.cam));
        let n = f.styles.len().min(MAX_STYLES);
        if f.styles.len() > MAX_STYLES && !STYLE_OVERFLOW.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("lsv: {} drawing styles, only the first {} have their own colour", f.styles.len(), MAX_STYLES);
        }
        queue.write_buffer(&self.style_buf, 0, bytemuck::cast_slice(&f.styles[..n]));
        self.extra = if f.extra.is_empty() { None } else { Some((device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("extra"), contents: bytemuck::cast_slice(&f.extra), usage: wgpu::BufferUsages::VERTEX }), f.extra.len() as u32)) };
        if f.hl_version != self.hl_version {
            self.hl = if f.hl.is_empty() { None } else { Some((device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("hl"), contents: bytemuck::cast_slice(&f.hl[..]), usage: wgpu::BufferUsages::VERTEX }), f.hl.len() as u32)) };
            self.hl_version = f.hl_version;
        }
    }

    pub fn paint(&self, pass: &mut wgpu::RenderPass<'_>, f: &Frame) {
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.bind_group, &[]);
        let draw_range = |pass: &mut wgpu::RenderPass<'_>, chunks: &[(wgpu::Buffer, u32)], a: u32, b: u32| {
            let mut base = 0u32;
            for (buf, n) in chunks {
                let (ca, cb) = (a.max(base), b.min(base + n));
                if ca < cb {
                    pass.set_vertex_buffer(0, buf.slice(..));
                    pass.draw(0..4, (ca - base)..(cb - base));
                }
                base += n;
            }
        };
        // world box in view (2 px margin: tiny shapes are drawn at least one pixel wide)
        let c = &f.cam;
        let s = (c.scale as f64).max(1e-12);
        let (hx, hy, m) = (c.size[0] as f64 / (2.0 * s), c.size[1] as f64 / (2.0 * s), 2.0 / s);
        let view = [c.origin[0] as f64 - hx - m, c.origin[1] as f64 - hy - m, c.origin[0] as f64 + hx + m, c.origin[1] as f64 + hy + m];
        // zoomed far out on a large layout: the overview, whose bins are about a pixel
        let ns = self.slot_range.len();
        let ov_styles = overview_style_base(ns) as usize + ns;
        let use_ov = !self.ov.is_empty() && self.ov_bin * s <= OV_MAX_PX && ov_styles <= MAX_STYLES && f.styles.len() >= ov_styles;
        let mut drawn = 0u64;
        let mut ranges = vec![];
        for (slot, &(a, b)) in self.slot_range.iter().enumerate() {
            if a >= b || !f.visible.get(slot).copied().unwrap_or(false) {
                continue;
            }
            ranges.clear();
            if use_ov {
                if let Some(&r) = self.ov_range.get(slot) {
                    ranges.push(r);
                }
            } else if let Some(t) = &self.tiles {
                t.ranges(slot, view, &mut ranges);
            } else {
                ranges.push((a, b));
            }
            for &(ra, rb) in &ranges {
                if ra < rb {
                    draw_range(pass, if use_ov { &self.ov } else { &self.chunks }, ra, rb);
                    drawn += (rb - ra) as u64;
                }
            }
        }
        self.drawn.store(drawn, Ordering::Relaxed);
        self.drew_overview.store(use_ov, Ordering::Relaxed);
        // test hook: LSV_DRAW_STATS=1 prints what a frame draws, at most once a second
        if !self.chunks.is_empty() && std::env::var("LSV_DRAW_STATS").is_ok() {
            static LAST: AtomicU64 = AtomicU64::new(0);
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
            if LAST.swap(now, Ordering::Relaxed) != now {
                eprintln!("lsv: draw {} instances{} (scale {:.3e} px per unit)", drawn, if use_ov { " from the overview" } else if self.tiles.is_some() { " in view" } else { "" }, s);
            }
        }
        if let Some((buf, n)) = &self.extra {
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..4, 0..*n);
        }
        if let Some((buf, n)) = &self.hl {
            pass.set_vertex_buffer(0, buf.slice(..));
            pass.draw(0..4, 0..*n);
        }
    }
}

/// style table for a scene: slot 0 cells, layer slots, then highlight fill, veil, die outline, accent outline
pub struct StyleTable {
    pub styles: Vec<Style>,
    pub hl: u32,
    pub veil: u32,
    pub die: u32,
    pub accent_line: u32,
}

pub fn styles_for(scene: &Scene, accent: [f32; 3], bg: [f32; 3], fg: [f32; 3]) -> StyleTable {
    let mut v = vec![Style { color: [0.55, 0.60, 0.66, 1.0], p: [0.10, 0.0, 0.0, 0.0] }];
    for st in &scene.styles {
        let s = Style { color: [st.color[0], st.color[1], st.color[2], 1.0], p: [0.2, st.hatch as f32, if st.cut { 1.0 } else { 0.0 }, 0.0] };
        for _ in 0..crate::scene::GROUPS {
            v.push(s);
        }
    }
    let hl = v.len() as u32;
    v.push(Style { color: [accent[0], accent[1], accent[2], 1.0], p: [0.75, 0.0, 0.0, 0.0] });
    let veil = v.len() as u32;
    v.push(Style { color: [bg[0], bg[1], bg[2], 1.0], p: [0.62, 0.0, 4.0, 0.0] });
    let die = v.len() as u32;
    v.push(Style { color: [fg[0], fg[1], fg[2], 1.0], p: [0.0, 0.0, 2.0, 0.0] });
    let accent_line = v.len() as u32;
    v.push(Style { color: [accent[0], accent[1], accent[2], 1.0], p: [0.0, 0.0, 2.0, 0.0] });
    // overview styles, one per slot (plain fill), when they fit in the style table
    let ns = scene.slot_range.len();
    if v.len() as u32 == overview_style_base(ns) && v.len() + ns <= MAX_STYLES {
        for k in 0..ns {
            v.push(Style { color: v[k].color, p: [if k == 0 { 0.25 } else { 0.85 }, 0.0, 4.0, 0.0] });
        }
    }
    StyleTable { styles: v, hl, veil, die, accent_line }
}
