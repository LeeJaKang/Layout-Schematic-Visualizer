//! Renders a layout to a PNG without a window, with the same GPU renderer as the viewer.
//! usage: lsv-render --def <def> --lef <lef>... --out <png> [--size 1600x1000] [--view x1,y1,x2,y2] [--comp NAME] [--net NAME]
use eframe::wgpu;
use lsv::def::parse_def;
use lsv::gpu::{styles_for, Cam, Frame, Gpu};
use lsv::lef::{parse_lef, Lib};
use lsv::scene::{Inst, Scene};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut def, mut lefs, mut out, mut size, mut view, mut comp, mut net) = (None, vec![], PathBuf::from("layout.png"), (1600u32, 1000u32), None, None, None);
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--def" => def = Some(PathBuf::from(v)),
            "--lef" => lefs.push(PathBuf::from(v)),
            "--out" => out = PathBuf::from(v),
            "--size" => { let p: Vec<u32> = v.split('x').map(|x| x.parse().unwrap()).collect(); size = (p[0], p[1]); }
            "--view" => view = Some(v.split(',').map(|x| x.parse::<f64>().unwrap()).collect::<Vec<f64>>()),
            "--comp" => comp = Some(v),
            "--net" => net = Some(v),
            a => panic!("unknown argument {}", a),
        }
        i += 2;
    }
    let t0 = Instant::now();
    let mut lib = Lib::default();
    for f in &lefs {
        parse_lef(&lsv::io::read_all(f).unwrap(), &f.display().to_string(), &mut lib).unwrap();
    }
    let def = def.expect("need --def");
    let text = lsv::io::read_all(&def).unwrap();
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let lay = parse_def(&text, &def.display().to_string(), &lib, threads);
    drop(text);
    let t1 = Instant::now();
    let scene = Scene::build(&lay, &lib, false);
    let t2 = Instant::now();
    println!("read {:.2} s, scene {:.2} s: {} instances", (t1 - t0).as_secs_f64(), (t2 - t1).as_secs_f64(), scene.inst.len());

    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).expect("no GPU adapter");
    println!("adapter: {:?}", adapter.get_info().name);
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut gpu = Gpu::new(&device, format);
    gpu.upload(&device, &scene);
    let t3 = Instant::now();

    let (w, h) = size;
    let vb = view.as_ref().map(|v| [v[0], v[1], v[2], v[3]]).unwrap_or([scene.die[0] as f64, scene.die[1] as f64, scene.die[2] as f64, scene.die[3] as f64]);
    let mut vb = vb;
    let mut hl: Vec<Inst> = vec![];
    let tb = styles_for(&scene, [0.83, 0.31, 0.06], [1.0, 1.0, 1.0], [0.09, 0.13, 0.18]);
    let (styles, hl_slot, veil_slot, die_slot, acc_slot) = (tb.styles, tb.hl, tb.veil, tb.die, tb.accent_line);
    if let Some(c) = &comp {
        let ci = *lay.comp_index.get(c).expect("no such component") as usize;
        let cp = &lay.comps[ci];
        if view.is_none() {
            let m = cp.h * 6.0;
            vb = [cp.x - m, cp.y - m, cp.x + cp.w + m, cp.y + cp.h + m];
        }
        for (ly, _, r) in lsv::def::comp_shapes(&lay, &lib, cp) {
            if let Some(&li) = lay.layer_ids.get(&ly) {
                hl.push(Inst { r: [r[0] as i32, r[1] as i32, r[2] as i32, r[3] as i32], slot: lsv::scene::slot_of(li, 0) });
            }
        }
        hl.push(Inst { r: [cp.x as i32, cp.y as i32, (cp.x + cp.w) as i32, (cp.y + cp.h) as i32], slot: acc_slot });
    }
    if let Some(n) = &net {
        let ni = *lay.net_index.get(n).expect("no such net") as usize;
        for &k in &scene.net_items[scene.net_start[ni] as usize..scene.net_start[ni + 1] as usize] {
            hl.push(Inst { r: lay.rects.r[k as usize], slot: hl_slot });
        }
    }
    let scale = ((w as f64 / (vb[2] - vb[0])).min(h as f64 / (vb[3] - vb[1]))) * 0.95;
    let origin = [((vb[0] + vb[2]) / 2.0).round() as i32, ((vb[1] + vb[3]) / 2.0).round() as i32];
    let big = [i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4];
    let mut extra = vec![Inst { r: scene.die, slot: die_slot }];
    if !hl.is_empty() {
        extra.push(Inst { r: big, slot: veil_slot });
    }
    let mut visible = vec![true; scene.slot_range.len()];
    for (li, st) in scene.styles.iter().enumerate() {
        for g in 0..lsv::scene::GROUPS {
            visible[lsv::scene::slot_of(li as u8, g) as usize] = st.visible;
        }
    }
    let frame = Frame { cam: Cam { origin, size: [w as f32, h as f32], scale: scale as f32, dpr: 1.0, pad: [0.0; 2] }, styles, visible, extra, veil_from: 1, hl: Arc::new(hl), hl_version: 1 };
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
    let t4 = Instant::now();
    queue.submit([enc.finish()]);
    readback.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let t5 = Instant::now();
    let data = readback.slice(..).get_mapped_range().expect("map readback buffer");
    let mut img = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        img.extend_from_slice(&data[(y * bpr) as usize..(y * bpr + w * 4) as usize]);
    }
    let file = std::fs::File::create(&out).unwrap();
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.write_header().unwrap().write_image_data(&img).unwrap();
    println!("upload {:.2} s, draw+readback {:.3} s -> {}", (t3 - t2).as_secs_f64(), (t5 - t4).as_secs_f64(), out.display());
}
