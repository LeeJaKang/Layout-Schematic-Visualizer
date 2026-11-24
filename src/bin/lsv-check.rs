//! Reads LEF/DEF with the desktop readers, reports timing, a checksum of all shapes (to compare with the
//! web viewer's reader), and optionally compares layer areas and instance boxes with a KLayout dump.
//! usage: lsv-check --def <file.def[.gz]> --lef <tech.lef> [--lef ...] [--klayout dump.json] [--threads N]
//!   [--mem] memory by stage, [--scene] build the viewer's scene, [--scene-full] also check it against the whole-array
//!   scene, [--objects] hash of components, nets and pins, [--in-memory] read the whole text first (the old way)
use lsv::def::{comp_shapes, parse_def, parse_def_file, Layout};
use lsv::lef::{parse_lef, Lib};
use lsv::{K_BLOCK, K_IOPIN, K_SWIRE};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::time::Instant;

fn union_area(rects: &[[i64; 4]]) -> i64 {
    let mut ys: Vec<i64> = rects.iter().flat_map(|r| [r[1], r[3]]).collect();
    ys.sort_unstable();
    ys.dedup();
    if ys.len() < 2 {
        return 0;
    }
    let segs = ys.len() - 1;
    let yi = |v: i64| ys.binary_search(&v).unwrap();
    let mut ev: Vec<(i64, usize, usize, i32)> = Vec::with_capacity(rects.len() * 2);
    for r in rects {
        if r[2] <= r[0] || r[3] <= r[1] {
            continue;
        }
        let (a, b) = (yi(r[1]), yi(r[3]));
        ev.push((r[0], a, b, 1));
        ev.push((r[2], a, b, -1));
    }
    ev.sort_unstable_by_key(|e| e.0);
    let mut cnt = vec![0i32; 4 * segs + 4];
    let mut len = vec![0i64; 4 * segs + 4];
    fn upd(node: usize, l: usize, r: usize, a: usize, b: usize, v: i32, cnt: &mut [i32], len: &mut [i64], ys: &[i64]) {
        if b <= l || r <= a {
            return;
        }
        if a <= l && r <= b {
            cnt[node] += v;
        } else {
            let mid = (l + r) / 2;
            upd(2 * node, l, mid, a, b, v, cnt, len, ys);
            upd(2 * node + 1, mid, r, a, b, v, cnt, len, ys);
        }
        len[node] = if cnt[node] > 0 { ys[r] - ys[l] } else if r - l == 1 { 0 } else { len[2 * node] + len[2 * node + 1] };
    }
    let mut area = 0i64;
    let mut last_x = ev.first().map(|e| e.0).unwrap_or(0);
    for (x, a, b, v) in ev {
        area += len[1] * (x - last_x);
        last_x = x;
        upd(1, 0, segs, a, b, v, &mut cnt, &mut len, &ys);
    }
    area
}

fn checksum(lay: &Layout) -> u32 {
    let mut h: u32 = 2166136261;
    let mut put = |v: u32| {
        h ^= v;
        h = h.wrapping_mul(16777619);
    };
    let r = &lay.rects;
    for i in 0..r.len() {
        for k in 0..4 {
            put(r.r[i][k] as u32);
        }
        put(r.layer[i] as u32);
        put(r.kind[i] as u32);
        put(r.owner[i] as u32);
    }
    h
}

/// FNV-32 over everything else the reader keeps (rows, components, nets, pins, layer names, warnings), for --objects
fn object_hash(lay: &Layout) -> u32 {
    let mut h: u32 = 2166136261;
    let mut put = |b: &[u8]| {
        for &c in b.iter().chain(std::iter::once(&0xffu8)) {
            h ^= c as u32;
            h = h.wrapping_mul(16777619);
        }
    };
    let f = |v: f64| v.to_bits().to_le_bytes();
    put(lay.design.as_bytes());
    put(&f(lay.dbu));
    for v in lay.die { put(&f(v)); }
    for r in &lay.rows { put(r.name.as_bytes()); put(r.site.as_bytes()); put(r.orient.as_bytes()); for v in [r.x, r.y, r.w, r.h] { put(&f(v)); } }
    for c in &lay.comps { put(c.name.as_bytes()); put(c.model.as_bytes()); put(c.orient.as_bytes()); put(c.status.as_bytes()); for v in [c.x, c.y, c.w, c.h] { put(&f(v)); } }
    for n in &lay.nets { put(n.name.as_bytes()); put(&[n.special as u8]); put(n.usage.as_bytes()); for (c, p) in &n.conns { put(c.as_bytes()); put(p.as_bytes()); } }
    for p in &lay.pins { put(p.name.as_bytes()); put(p.net.as_deref().unwrap_or("-").as_bytes()); put(p.dir.as_bytes()); put(p.usage.as_bytes()); put(&(p.ports.len() as u32).to_le_bytes()); for pt in &p.ports { put(&(pt.shapes.len() as u32).to_le_bytes()); for v in [pt.x, pt.y] { put(&f(v)); } put(pt.orient.as_bytes()); } }
    for nm in &lay.layer_names { put(nm.as_bytes()); }
    let mut vias: Vec<(&String, usize)> = lay.vias.iter().map(|(k, v)| (k, v.shapes.len())).collect();
    vias.sort();
    for (k, n) in vias { put(k.as_bytes()); put(&(n as u32).to_le_bytes()); }
    for w in &lay.warnings { put(w.as_bytes()); }
    let mut ci: Vec<(&String, &u32)> = lay.comp_index.iter().collect();
    ci.sort();
    for (k, v) in ci { put(k.as_bytes()); put(&v.to_le_bytes()); }
    h
}

/// estimated heap bytes of the main layout parts (glibc chunk sizes), for --mem
fn layout_bytes(lay: &Layout) {
    let s = |x: &String| if x.capacity() == 0 { 24 } else { 24 + ((x.capacity() + 8 + 15) & !15).max(32) };
    let mb = |v: usize| v / 1_000_000;
    let rects = lay.rects.r.capacity() * 16 + lay.rects.layer.capacity() + lay.rects.owner.capacity() * 4 + lay.rects.kind.capacity();
    let comps: usize = lay.comps.capacity() * (std::mem::size_of::<lsv::def::Comp>() - 96) + lay.comps.iter().map(|c| s(&c.name) + s(&c.model) + s(&c.orient) + s(&c.status)).sum::<usize>();
    let nconn: usize = lay.nets.iter().map(|n| n.conns.len()).sum();
    let conns: usize = lay.nets.iter().map(|n| n.conns.capacity() * 48 + n.conns.iter().map(|(c, p)| s(c) + s(p) - 48).sum::<usize>()).sum();
    let nets: usize = lay.nets.capacity() * std::mem::size_of::<lsv::def::Net>() + lay.nets.iter().map(|n| s(&n.name) + s(&n.usage) - 48).sum::<usize>();
    let idx = |m: &HashMap<String, u32>| m.capacity() * 33 + m.keys().map(|k| s(k) - 24).sum::<usize>();
    println!("mem estimate: rects {} MB ({} rects), comps {} MB, nets {} MB, conns {} MB ({} conns), comp_index {} MB, net_index {} MB", mb(rects), lay.rects.len(), mb(comps), mb(nets), mb(conns), nconn, mb(idx(&lay.comp_index)), mb(idx(&lay.net_index)));
}

/// checks of the drawing data of a lean scene against the whole-array scene: the same instances in every slot
/// (order free), no instance in view left out by the tile ranges, and an overview equal to the covered bins
fn verify_large(sc: &lsv::scene::Scene, lay: &Layout, lib: &Lib) {
    use lsv::scene::{Inst, Scene, OV_BINS};
    let mix = |x: &Inst| {
        let mut z = (x.r[0] as u32 as u64) ^ ((x.r[1] as u32 as u64) << 32);
        z = z.wrapping_mul(0x9E3779B97F4A7C15) ^ ((x.r[2] as u32 as u64) | ((x.r[3] as u32 as u64) << 32));
        z = (z ^ (z >> 31)).wrapping_mul(0xBF58476D1CE4E5B9) ^ x.slot as u64;
        (z ^ (z >> 29)).wrapping_mul(0x94D049BB133111EB)
    };
    let total = sc.inst_count();
    let lean = sc.lean_insts(lay, 0, total);
    let full = Scene::build(lay, lib, false);
    let ns = sc.slot_range.len();
    let (mut sa, mut sb) = (vec![0u64; ns], vec![0u64; ns]);
    for (s, &(a, b)) in sc.slot_range.iter().enumerate() {
        for i in a as usize..b as usize {
            sa[s] = sa[s].wrapping_add(mix(&lean[i]));
            sb[s] = sb[s].wrapping_add(mix(&full.inst[i]));
        }
    }
    let same_slots = lean.iter().zip(&full.inst).all(|(x, y)| x.slot == y.slot);
    println!("check: same instances in every slot as Scene::build (order free): {} of {} slots, slots in place {}, same order {}", (0..ns).filter(|&s| sa[s] == sb[s]).count(), ns, same_slots, lean.iter().zip(&full.inst).all(|(x, y)| x.r == y.r && x.slot == y.slot));
    drop(full);
    if let Some(t) = &sc.tiles {
        let d = [sc.die[0] as f64, sc.die[1] as f64, sc.die[2] as f64, sc.die[3] as f64];
        let (w, h) = (d[2] - d[0], d[3] - d[1]);
        let views = [d, [d[0], d[1], d[0] + w / 2.0, d[1] + h / 2.0], [d[0] + w * 0.45, d[1] + h * 0.45, d[0] + w * 0.55, d[1] + h * 0.55], [d[0] + w * 0.3, d[1] + h * 0.7, d[0] + w * 0.305, d[1] + h * 0.705], [d[0] - w * 0.1, d[1] - h * 0.1, d[0] + w * 0.02, d[1] + h * 0.03], [d[2] - w * 0.01, d[3] - h * 0.01, d[2] + w * 0.1, d[3] + h * 0.1]];
        let mut inc = vec![false; total];
        let mut ranges = vec![];
        for v in views {
            inc.iter_mut().for_each(|x| *x = false);
            let mut drawn = 0usize;
            for s in 0..ns {
                ranges.clear();
                t.ranges(s, v, &mut ranges);
                for &(a, b) in &ranges {
                    for x in &mut inc[a as usize..b as usize] {
                        *x = true;
                    }
                    drawn += (b - a) as usize;
                }
            }
            let (mut seen, mut missed) = (0usize, 0usize);
            for (i, x) in lean.iter().enumerate() {
                let hit = (x.r[2] as f64) >= v[0] && (x.r[0] as f64) <= v[2] && (x.r[3] as f64) >= v[1] && (x.r[1] as f64) <= v[3];
                if hit {
                    seen += 1;
                    if !inc[i] {
                        missed += 1;
                    }
                }
            }
            println!("check: view {:?}: {} instances touch it, {} drawn, {} missed", v.map(|z| z as i64), seen, drawn, missed);
        }
    }
    if let Some(o) = &sc.overview {
        let n = OV_BINS;
        let (w, h) = ((sc.die[2] - sc.die[0]).max(1) as i64, (sc.die[3] - sc.die[1]).max(1) as i64);
        let (bw, bh) = (((w + n as i64 - 1) / n as i64).max(1), ((h + n as i64 - 1) / n as i64).max(1));
        let (x0, y0) = (sc.die[0] as i64, sc.die[1] as i64);
        let cx = |x: i64| ((x - x0) / bw).clamp(0, n as i64 - 1) as usize;
        let cy = |y: i64| ((y - y0) / bh).clamp(0, n as i64 - 1) as usize;
        let mut bad = 0;
        for (s, &(a, b)) in sc.slot_range.iter().enumerate() {
            let (mut want, mut got) = (vec![false; n * n], vec![false; n * n]);
            for x in &lean[a as usize..b as usize] {
                for j in cy(x.r[1] as i64)..=cy((x.r[3] - 1).max(x.r[1]) as i64) {
                    for i in cx(x.r[0] as i64)..=cx((x.r[2] - 1).max(x.r[0]) as i64) {
                        want[j * n + i] = true;
                    }
                }
            }
            let (oa, ob) = o.slot_range[s];
            for x in &o.inst[oa as usize..ob as usize] {
                for j in ((x.r[1] as i64 - y0) / bh) as usize..((x.r[3] as i64 - y0) / bh) as usize {
                    for i in ((x.r[0] as i64 - x0) / bw) as usize..((x.r[2] as i64 - x0) / bw) as usize {
                        if got[j * n + i] {
                            bad += 1;
                        }
                        got[j * n + i] = true;
                    }
                }
            }
            if want != got {
                bad += 1;
                println!("check: overview of slot {} differs from the covered bins", s);
            }
        }
        println!("check: overview equal to the covered bins in all {} slots: {} (problems {})", ns, bad == 0, bad);
    }
}

/// resident and peak memory of this process in MB (Linux), for --mem
fn mem_line(stage: &str) {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |k: &str| s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    println!("mem {:<12} rss {:>7} MB  peak {:>7} MB", stage, kb("VmRSS:") / 1024, kb("VmHWM:") / 1024);
}

fn main() {
    let mut def = None;
    let mut lefs = vec![];
    let mut kl = None;
    let (mut mem, mut scene, mut in_memory, mut objects, mut scene_full) = (false, false, false, false, false);
    let mut threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--def" => { def = Some(PathBuf::from(&args[i + 1])); i += 1; }
            "--lef" => { lefs.push(PathBuf::from(&args[i + 1])); i += 1; }
            "--klayout" => { kl = Some(PathBuf::from(&args[i + 1])); i += 1; }
            "--threads" => { threads = args[i + 1].parse().unwrap(); i += 1; }
            "--mem" => mem = true,
            "--scene" => scene = true,
            "--in-memory" => in_memory = true,
            "--objects" => objects = true,
            "--scene-full" => { scene = true; scene_full = true; }
            a => { eprintln!("unknown argument {}", a); std::process::exit(2); }
        }
        i += 1;
    }
    rayon::ThreadPoolBuilder::new().num_threads(threads).build_global().unwrap();
    let def = def.expect("need --def");
    let t0 = Instant::now();
    let mut lib = Lib::default();
    for f in &lefs {
        let text = lsv::io::read_all(f).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
        parse_lef(&text, &f.display().to_string(), &mut lib).unwrap_or_else(|e| panic!("{}", e));
    }
    let t1 = Instant::now();
    if mem { mem_line("lef"); }
    // default: the file is read block by block while it is parsed; --in-memory reads the whole text first
    let (lay, text_len, t2) = if in_memory {
        let text = lsv::io::read_all(&def).unwrap_or_else(|e| panic!("{}: {}", def.display(), e));
        let t2 = Instant::now();
        if mem { mem_line("def text"); }
        (parse_def(&text, &def.display().to_string(), &lib, threads), text.len() as u64, Some(t2))
    } else {
        let (lay, n) = parse_def_file(&def, &lib, threads).unwrap_or_else(|e| panic!("{}: {}", def.display(), e));
        (lay, n, None)
    };
    let t3 = Instant::now();
    if mem { mem_line("parsed"); layout_bytes(&lay); }
    println!("LEF: {} macros, {} layers, {:.2} s", lib.macros.len(), lib.layers.len(), (t1 - t0).as_secs_f64());
    match t2 {
        Some(t2) => println!("DEF read+gunzip: {} MB text, {:.2} s | parse with {} threads: {:.2} s", text_len / 1_000_000, (t2 - t1).as_secs_f64(), threads, (t3 - t2).as_secs_f64()),
        None => println!("DEF read+gunzip+parse (streamed): {} MB text, {} threads: {:.2} s", text_len / 1_000_000, threads, (t3 - t1).as_secs_f64()),
    }
    println!("DEF {}: {} comps, {} nets, {} pins, {} rects, {} layers, warnings {}", lay.design, lay.comps.len(), lay.nets.len(), lay.pins.len(), lay.rects.len(), lay.layer_names.len(), lay.warnings.len());
    for w in lay.warnings.iter().take(5) {
        println!("  warning: {}", w);
    }
    println!("shape checksum (FNV-32 over x1 y1 x2 y2 layer kind owner): {:08x}", checksum(&lay));
    if objects {
        println!("object hash (rows, components, nets, pins, layers, vias, warnings): {:08x}", object_hash(&lay));
    }
    if scene {
        // what the viewer builds after parsing
        let t4 = Instant::now();
        let mut sc = lsv::scene::Scene::build_lean(&lay, &lib, false);
        // search index, as app.rs load_layout builds it
        use rayon::prelude::*;
        let mut names: Vec<(String, u8, u32)> = lay.comps.par_iter().enumerate().map(|(i, c)| (c.name.to_lowercase(), 0u8, i as u32)).collect();
        names.par_extend(lay.nets.par_iter().enumerate().map(|(i, n)| (n.name.to_lowercase(), 1u8, i as u32)));
        names.par_sort_by(|a, b| a.0.cmp(&b.0));
        println!("scene: {} instances, grid {}x{} with {} entries, {} names, {:.2} s", sc.inst_count(), sc.grid.nx, sc.grid.ny, sc.grid.items.len(), names.len(), t4.elapsed().as_secs_f64());
        if mem {
            mem_line("scene");
            let s = |x: &String| if x.capacity() == 0 { 0 } else { ((x.capacity() + 8 + 15) & !15).max(32) };
            let cnp = sc.comp_net_pins.len() * 4 + sc.comp_net_pins.entries() * 24;
            let nm: usize = names.capacity() * 32 + names.iter().map(|(n, _, _)| s(n)).sum::<usize>();
            println!("mem estimate: scene order {} MB, grid {} MB, net items {} MB, comp pins {} MB, names {} MB", sc.order.capacity() * 4 / 1_000_000, (sc.grid.items.capacity() + sc.grid.start.capacity() + sc.grid.citems.capacity() + sc.grid.cstart.capacity()) * 4 / 1_000_000, (sc.net_items.capacity() + sc.net_start.capacity()) * 4 / 1_000_000, cnp / 1_000_000, nm / 1_000_000);
        }
        // the instances the viewer uploads, chunk by chunk as Gpu::upload_layout makes them
        let t5 = Instant::now();
        let (mut ih, total) = (2166136261u32, sc.inst_count());
        let mut hash_insts = |v: &[lsv::scene::Inst]| {
            for x in v {
                for w in [x.r[0] as u32, x.r[1] as u32, x.r[2] as u32, x.r[3] as u32, x.slot] {
                    ih ^= w;
                    ih = ih.wrapping_mul(16777619);
                }
            }
        };
        let mut a = 0;
        while a < total {
            let b = (a + (6 << 20)).min(total);
            hash_insts(&sc.lean_insts(&lay, a, b));
            a = b;
        }
        println!("GPU instance hash (chunked from the layout): {:08x}, {:.2} s", ih, t5.elapsed().as_secs_f64());
        if let Some(o) = &sc.overview {
            println!("overview: {} rectangles for {} instances, bin {} units", o.inst.len(), total, o.bin);
        }
        if mem { mem_line("upload"); }
        if scene_full {
            verify_large(&sc, &lay, &lib);
        }
        sc.order = Vec::new();
        if scene_full {
            // the old path: the whole instance array built at once
            let full = lsv::scene::Scene::build(&lay, &lib, false);
            ih = 2166136261;
            let mut h2 = |v: &[lsv::scene::Inst]| {
                for x in v {
                    for w in [x.r[0] as u32, x.r[1] as u32, x.r[2] as u32, x.r[3] as u32, x.slot] {
                        ih ^= w;
                        ih = ih.wrapping_mul(16777619);
                    }
                }
            };
            h2(&full.inst);
            println!("GPU instance hash (Scene::build, whole array): {:08x}, same slot ranges {}", ih, full.slot_range == sc.slot_range);
            // reference: the grid, net and pin lists filled one item after another (the code before the parallel fill)
            let g = &sc.grid;
            let seq = |list: &[[i32; 4]]| -> (Vec<u32>, Vec<u32>) {
                let mut counts = vec![0u32; g.nx * g.ny + 1];
                for b in list {
                    let (i0, j0, i1, j1) = g.cell_range(b[0] as i64, b[1] as i64, b[2] as i64, b[3] as i64);
                    for j in j0..=j1 { for i in i0..=i1 { counts[j * g.nx + i + 1] += 1; } }
                }
                for k in 1..counts.len() { counts[k] += counts[k - 1]; }
                let mut fillp = counts.clone();
                let mut items = vec![0u32; *counts.last().unwrap() as usize];
                for (idx, b) in list.iter().enumerate() {
                    let (i0, j0, i1, j1) = g.cell_range(b[0] as i64, b[1] as i64, b[2] as i64, b[3] as i64);
                    for j in j0..=j1 { for i in i0..=i1 { let c = j * g.nx + i; items[fillp[c] as usize] = idx as u32; fillp[c] += 1; } }
                }
                (counts, items)
            };
            let (s1, i1) = seq(&lay.rects.r);
            let cb: Vec<[i32; 4]> = lay.comps.iter().map(|c| [c.x as i32, c.y as i32, (c.x + c.w) as i32, (c.y + c.h) as i32]).collect();
            let (s2, i2) = seq(&cb);
            let r = &lay.rects;
            let mut st = vec![0u32; lay.nets.len() + 1];
            let is_net = |k: usize| matches!(r.kind[k], lsv::K_WIRE | lsv::K_VIA | lsv::K_SWIRE | lsv::K_SVIA) && r.owner[k] >= 0;
            for k in 0..r.len() { if is_net(k) { st[r.owner[k] as usize + 1] += 1; } }
            for k in 1..st.len() { st[k] += st[k - 1]; }
            let mut fp = st.clone();
            let mut ni = vec![0u32; *st.last().unwrap() as usize];
            for k in 0..r.len() { if is_net(k) { let o = r.owner[k] as usize; ni[fp[o] as usize] = k as u32; fp[o] += 1; } }
            let mut pins_ok = true;
            let mut want: Vec<Vec<(String, u32)>> = vec![vec![]; lay.comps.len()];
            for (n, net) in lay.nets.iter().enumerate() {
                for (c, p) in &net.conns {
                    if c != "PIN" && c != "*" { if let Some(&ci) = lay.comp_index.get(c) { want[ci as usize].push((p.clone(), n as u32)); } }
                }
            }
            for (ci, w) in want.iter().enumerate() {
                let got = &sc.comp_net_pins[ci];
                if got.len() != w.len() || got.iter().zip(w).any(|(a, b)| &*a.0 != b.0.as_str() || a.1 != b.1) { pins_ok = false; }
            }
            println!("parallel lists equal to the sequential fill: grid {} components {} nets {} component pins {}", s1 == g.start && i1 == g.items, s2 == g.cstart && i2 == g.citems, st == sc.net_start && ni == sc.net_items, pins_ok);
        }
        drop(sc);
        drop(names);
    }
    if let Some(kl) = kl {
        let j: serde_json::Value = serde_json::from_slice(&std::fs::read(&kl).unwrap()).unwrap();
        let mut maps: [HashMap<String, Vec<[i64; 4]>>; 4] = Default::default(); // route special defpin lefpin
        let r = &lay.rects;
        for k in 0..r.len() {
            let nm = &lay.layer_names[r.layer[k] as usize];
            let rr = r.r[k].map(|v| v as i64);
            let kd = r.kind[k];
            if kd == K_BLOCK {
                if nm == "(placement)" {
                    maps[0].entry("PLACEMENT_BLK".into()).or_default().push(rr);
                }
                continue;
            }
            let m = if kd == K_IOPIN { 2 } else if kd == K_SWIRE { 1 } else { 0 };
            maps[m].entry(nm.clone()).or_default().push(rr);
        }
        for c in &lay.comps {
            for (ly, pin, rr) in comp_shapes(&lay, &lib, c) {
                if pin.is_some() {
                    maps[3].entry(ly).or_default().push(rr.map(|v| (v + 0.5).floor() as i64));
                }
            }
        }
        let layers = j["layers"].as_object().unwrap();
        let (mut rows, mut bad) = (0, 0);
        for (mi, (label, suffix)) in [("route", ""), ("special", ".SPECIAL"), ("defpin", ".PIN"), ("lefpin", ".LEFPIN")].iter().enumerate() {
            let mut names: BTreeSet<String> = maps[mi].keys().cloned().collect();
            for k in layers.keys() {
                if suffix.is_empty() { if !k.contains('.') { names.insert(k.clone()); } } else if let Some(b) = k.strip_suffix(suffix) { names.insert(b.to_string()); }
            }
            for nm in names {
                let ours = maps[mi].get(&nm).map(|v| union_area(v)).unwrap_or(0);
                let theirs = layers.get(&format!("{}{}", nm, suffix)).and_then(|v| v["area"].as_i64()).unwrap_or(0);
                rows += 1;
                if ours != theirs { bad += 1; }
                println!("{:8} {:13} ours {:>14} klayout {:>14} {}", label, nm, ours, theirs, if ours == theirs { "EQUAL".to_string() } else { format!("DIFF {}", ours - theirs) });
            }
        }
        let inst = j["instances"].as_object().unwrap();
        let (mut ok, mut diff, mut missing) = (0, 0, 0);
        for c in &lay.comps {
            match inst.get(&c.name).and_then(|v| v.as_array()) {
                None => missing += 1,
                Some(b) => {
                    let b: Vec<f64> = b.iter().map(|v| v.as_f64().unwrap()).collect();
                    if c.x == b[0] && c.y == b[1] && c.x + c.w == b[2] && c.y + c.h == b[3] { ok += 1 } else { diff += 1 }
                }
            }
        }
        println!("instances: {} ours, {} equal boxes, {} different, {} not found in KLayout", lay.comps.len(), ok, diff, missing);
        if bad == 0 && diff == 0 && missing == 0 {
            println!("ALL EQUAL: {} layer areas and {} instance boxes match KLayout", rows, ok);
        } else {
            println!("MISMATCH: {} of {} layer areas differ, {} boxes differ, {} missing", bad, rows, diff, missing);
        }
    }
}
