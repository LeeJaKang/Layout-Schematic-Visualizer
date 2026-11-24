//! Renders the netlist overview (whole design) to a PNG without a window, with the viewer's GPU renderer
//! and the same instances, styles and camera as the netlist tab.
//! usage: lsv-schrender --lib <lib>... --netlist <v> [--top T] --out <png> [--size 1600x1000] [--zoom Z --center X,Y] [--select-net NAME | --select-cell NAME]
use eframe::egui::{Color32, Pos2, Rect, Vec2};
use eframe::wgpu;
use lsv::cells::analyze_cell;
use lsv::gpu::{Frame, Gpu};
use lsv::liberty::parse_liberty;
use lsv::netlist::{elaborate, pick_top};
use lsv::schematic::{cells_in_view, gpu_cam, gpu_hl, gpu_insts, gpu_styles, layout, Colors, Hit, View};
use lsv::verilog::parse_verilog;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |k: &str| args.iter().position(|x| x == k).and_then(|i| args.get(i + 1)).cloned();
    let libs: Vec<String> = args.iter().enumerate().filter(|(_, a)| *a == "--lib").filter_map(|(i, _)| args.get(i + 1).cloned()).collect();
    let net = get("--netlist").expect("--netlist");
    let out = get("--out").unwrap_or_else(|| "netlist.png".into());
    let (w, h) = get("--size").map(|s| {
        let p: Vec<u32> = s.split('x').map(|x| x.parse().unwrap()).collect();
        (p[0], p[1])
    }).unwrap_or((1600, 1000));
    let t0 = Instant::now();
    let mut models = HashMap::new();
    for f in &libs {
        for c in parse_liberty(&lsv::io::read_all(std::path::Path::new(f)).unwrap(), f).unwrap() {
            models.insert(c.name.clone(), Arc::new(analyze_cell(&c)));
        }
    }
    let (order, mods) = parse_verilog(&lsv::io::read_all(std::path::Path::new(&net)).unwrap(), &net).unwrap();
    let top = get("--top").unwrap_or_else(|| pick_top(&order, &mods)[0].clone());
    let nl = elaborate(&mods, &top, &models).unwrap();
    let t1 = Instant::now();
    let cells: Vec<u32> = (0..nl.cells.len() as u32).collect();
    let lay = layout(&nl, &cells);
    let t2 = Instant::now();
    let (inst, ranges) = gpu_insts(&lay, &nl);
    println!("{}: {} cells, {} nodes, {} wire pieces, sheet {:.0} x {:.0} | netlist {:.2} s, layout {:.2} s, {} GPU rectangles", nl.top, cells.len(), lay.nodes.len(), lay.wires.len(), lay.size.x, lay.size.y, (t1 - t0).as_secs_f64(), (t2 - t1).as_secs_f64(), inst.len());

    let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(w as f32, h as f32));
    let view = match (get("--zoom"), get("--center")) {
        (Some(z), Some(c)) => {
            let z: f32 = z.parse().unwrap();
            let c: Vec<f32> = c.split(',').map(|x| x.parse().unwrap()).collect();
            View { pan: Vec2::new(c[0], c[1]) - rect.size() / (2.0 * z), zoom: z }
        }
        _ => {
            let z = (rect.width() / lay.size.x).min(rect.height() / lay.size.y) * 0.95;
            View { pan: Vec2::new(lay.size.x / 2.0 - rect.width() / (2.0 * z), lay.size.y / 2.0 - rect.height() / (2.0 * z)), zoom: z }
        }
    };
    println!("view zoom {:.4}, cells in view {}", view.zoom, cells_in_view(&lay, &view, rect));
    let sel = if let Some(n) = get("--select-net") { nl.net_by_name.get(&n).map(|&x| Hit::Net(x)) } else if let Some(c) = get("--select-cell") { nl.cell_index.get(&c).map(|&x| Hit::Cell(x)) } else { None };
    let col = Colors { ink: Color32::from_rgb(23, 33, 43), wire: Color32::from_rgb(52, 73, 94), muted: Color32::from_rgb(90, 103, 114), hl: Color32::from_rgb(212, 80, 15), sel: Color32::from_rgb(28, 79, 140), fill: Color32::from_rgb(248, 249, 246) };

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).expect("no GPU adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut gpu = Gpu::new(&device, format);
    gpu.upload_raw(&device, &inst, ranges);
    let frame = Frame { cam: gpu_cam(&view, rect, 1.0), styles: gpu_styles(&col), visible: vec![true; lsv::schematic::GPU_SLOTS], extra: vec![], veil_from: 0, hl: Arc::new(gpu_hl(&lay, sel, view.zoom)), hl_version: 1 };
    gpu.prepare(&device, &queue, &frame);
    let tex = device.create_texture(&wgpu::TextureDescriptor { label: Some("out"), size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 }, mip_level_count: 1, sample_count: 1, dimension: wgpu::TextureDimension::D2, format, usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC, view_formats: &[] });
    let tv = tex.create_view(&Default::default());
    let bpr = (w * 4).div_ceil(256) * 256;
    let readback = device.create_buffer(&wgpu::BufferDescriptor { label: Some("read"), size: (bpr * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
    let mut enc = device.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("lsv"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment { view: &tv, depth_slice: None, resolve_target: None, ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::WHITE), store: wgpu::StoreOp::Store } })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        gpu.paint(&mut pass, &frame);
    }
    enc.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        wgpu::TexelCopyBufferInfo { buffer: &readback, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr), rows_per_image: Some(h) } },
        wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
    );
    let t3 = Instant::now();
    queue.submit([enc.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let t4 = Instant::now();
    let data = readback.slice(..).get_mapped_range().expect("map readback buffer");
    let mut img = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        img.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + w * 4) as usize]);
    }
    let file = std::fs::File::create(&out).unwrap();
    let mut pe = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    pe.set_color(png::ColorType::Rgba);
    pe.write_header().unwrap().write_image_data(&img).unwrap();
    println!("draw+readback {:.3} s -> {}", (t4 - t3).as_secs_f64(), out);
}
