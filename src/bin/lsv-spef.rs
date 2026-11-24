//! Reads a SPEF (block parasitics, such as OpenROAD OpenRCX output) with the DEF and LEF of the block, puts every
//! R and C on the metal segments of the routing, and checks the placement.
//! usage: lsv-spef <file.spef[.gz]> --def <file.def[.gz]> --lef <tech.lef> [--lef ...] [--net NAME]...
use lsv::def::parse_def_file;
use lsv::lef::{parse_lef, Lib};
use lsv::lpe::SegKind;
use lsv::spef::{net_segments, parse_spef};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage: lsv-spef <file.spef[.gz]> --def <file.def[.gz]> --lef <tech.lef> [--lef ...] [--net NAME]...");
    std::process::exit(2);
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut spef_p, mut def_p, mut lefs, mut show) = (None, None, vec![], vec![]);
    let mut i = 0;
    while i < a.len() {
        let next = |i: &mut usize| -> String {
            *i += 1;
            a.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match a[i].as_str() {
            "--def" => def_p = Some(PathBuf::from(next(&mut i))),
            "--lef" => lefs.push(PathBuf::from(next(&mut i))),
            "--net" => show.push(next(&mut i)),
            x if x.starts_with("--") => usage(),
            x => spef_p = Some(PathBuf::from(x)),
        }
        i += 1;
    }
    let (Some(spef_p), Some(def_p)) = (spef_p, def_p) else { usage() };
    let t0 = Instant::now();
    let text = String::from_utf8_lossy(&lsv::io::read_all(&spef_p).unwrap_or_else(|e| panic!("{}: {}", spef_p.display(), e))).into_owned();
    let sp = parse_spef(&text).unwrap_or_else(|e| panic!("{}: {}", spef_p.display(), e));
    let ncap: usize = sp.nets.iter().map(|n| n.caps.len()).sum();
    let ncc: usize = sp.nets.iter().map(|n| n.caps.iter().filter(|c| c.other.is_some()).count()).sum();
    let ncc_found: usize = sp.nets.iter().map(|n| n.caps.iter().filter(|c| c.other_at.is_some()).count()).sum();
    let nres: usize = sp.nets.iter().map(|n| n.res.len()).sum();
    println!(
        "SPEF {}: {} nets, {} nodes ({} with a location), {} R, {} C to ground, {} coupling C ({} with the other node found) | {:.2} s",
        sp.design,
        sp.nets.len(),
        sp.node_count,
        sp.located,
        nres,
        ncap - ncc,
        ncc,
        ncc_found,
        t0.elapsed().as_secs_f64()
    );
    let mut lib = Lib::default();
    for f in &lefs {
        let b = lsv::io::read_all(f).unwrap_or_else(|e| panic!("{}: {}", f.display(), e));
        parse_lef(&b, &f.display().to_string(), &mut lib).unwrap_or_else(|e| panic!("{}", e));
    }
    let (lay, _) = parse_def_file(Path::new(&def_p), &lib, 8).unwrap_or_else(|e| panic!("{}: {}", def_p.display(), e));
    println!("DEF {}: {} nets, {} shapes", lay.design, lay.nets.len(), lay.rects.len());
    let t1 = Instant::now();
    let (mut in_def, mut wire, mut via, mut no_seg, mut patch, mut caps_on, mut caps_off, mut ends_off, mut cap_pts_off) = (0, 0, 0, 0, 0, 0, 0, 0, 0);
    let (mut path, mut path_ends_off, mut via_apart) = (0, 0, 0);
    let mut off_by: std::collections::BTreeMap<&str, (usize, f64)> = Default::default();
    let mut why: std::collections::BTreeMap<String, usize> = Default::default();
    let tol = 0.5;
    let inside = |r: &[i32; 4], p: [f64; 2]| p[0] >= r[0] as f64 - tol && p[0] <= r[2] as f64 + tol && p[1] >= r[1] as f64 - tol && p[1] <= r[3] as f64 + tol;
    for n in &sp.nets {
        if lay.net_index.contains_key(&n.name) {
            in_def += 1;
        }
        let ns = net_segments(n, &lay, &lib);
        for s in &ns.segs {
            match s.kind {
                SegKind::Wire => {
                    wire += 1;
                    if !(s.rects.iter().any(|r| inside(r, s.ends[0])) && s.rects.iter().any(|r| inside(r, s.ends[1]))) {
                        ends_off += 1;
                    }
                }
                SegKind::Via => {
                    via += 1;
                    // a via whose two points are apart: no wire and no chain of shapes joins them
                    if ((s.ends[0][0] - s.ends[1][0]).powi(2) + (s.ends[0][1] - s.ends[1][1]).powi(2)).sqrt() > tol {
                        via_apart += 1;
                    }
                }
                SegKind::Patch => patch += 1,
                SegKind::Path => {
                    path += 1;
                    if !(s.rects.iter().any(|r| inside(r, s.ends[0])) && s.rects.iter().any(|r| inside(r, s.ends[1]))) {
                        path_ends_off += 1;
                        for e in 0..2 {
                            if !s.rects.iter().any(|r| inside(r, s.ends[e])) {
                                let nm = &n.nodes[s.nodes[e]].name;
                                let what = if !nm.contains(':') { "port" } else if nm.starts_with(&format!("{}:", n.name)) { "internal node" } else { "cell pin" };
                                let d = s.rects.iter().map(|r| {
                                    let q = [s.ends[e][0].clamp(r[0] as f64, r[2] as f64), s.ends[e][1].clamp(r[1] as f64, r[3] as f64)];
                                    ((q[0] - s.ends[e][0]).powi(2) + (q[1] - s.ends[e][1]).powi(2)).sqrt()
                                }).fold(f64::MAX, f64::min) / lay.dbu * 1000.0;
                                let t = off_by.entry(what).or_insert((0, 0.0));
                                t.0 += 1;
                                t.1 = f64::max(t.1, d);
                            }
                        }
                        if path_ends_off <= 4 {
                            for e in 0..2 {
                                if !s.rects.iter().any(|r| inside(r, s.ends[e])) {
                                    let d = s.rects.iter().map(|r| {
                                        let q = [s.ends[e][0].clamp(r[0] as f64, r[2] as f64), s.ends[e][1].clamp(r[1] as f64, r[3] as f64)];
                                        ((q[0] - s.ends[e][0]).powi(2) + (q[1] - s.ends[e][1]).powi(2)).sqrt()
                                    }).fold(f64::MAX, f64::min);
                                    println!("  path off its end: net {} R {} end {} at ({:.3}, {:.3}) um, {:.1} dbu from its shapes on {}", n.name, s.res.map(|k| n.res[k].name.clone()).unwrap_or_default(), n.nodes[s.nodes[e]].name, s.ends[e][0] / lay.dbu, s.ends[e][1] / lay.dbu, d, s.layer);
                                }
                            }
                        }
                    }
                }
            }
        }
        for r in &ns.res {
            if let Err(w) = r {
                no_seg += 1;
                *why.entry(w.clone()).or_default() += 1;
            }
        }
        for c in &ns.caps {
            match c {
                Some((s, p)) => {
                    caps_on += 1;
                    if !ns.segs[*s].rects.iter().any(|r| inside(r, *p)) {
                        cap_pts_off += 1;
                    }
                }
                None => caps_off += 1,
            }
        }
        if show.iter().any(|x| x == &n.name) {
            println!("net {}: total C {:.4} fF, {} nodes, {} R, {} C", n.name, n.total_f * 1e15, n.nodes.len(), n.res.len(), n.caps.len());
            for nd in &n.nodes {
                println!("  node {:<24} {}", nd.name, nd.xy.map(|p| format!("({:.3}, {:.3}) um", p[0], p[1])).unwrap_or_else(|| "-".into()));
            }
            for (k, r) in n.res.iter().enumerate() {
                match &ns.res[k] {
                    Ok(s) => {
                        let sg = &ns.segs[*s];
                        let len = ((sg.ends[0][0] - sg.ends[1][0]).powi(2) + (sg.ends[0][1] - sg.ends[1][1]).powi(2)).sqrt() / lay.dbu;
                        println!("  R {:<4} {:>10.3} ohm  {} - {}  {:?} on {} ({:.3} um, {} rects)", r.name, r.ohms, n.nodes[r.a].name, n.nodes[r.b].name, sg.kind, sg.layer, len, sg.rects.len());
                    }
                    Err(w) => println!("  R {:<4} {:>10.3} ohm  {} - {}  no segment: {}", r.name, r.ohms, n.nodes[r.a].name, n.nodes[r.b].name, w),
                }
            }
            for (k, c) in n.caps.iter().enumerate() {
                let side = ns.caps[k].map(|(s, _)| format!("{:?} on {}", ns.segs[s].kind, ns.segs[s].layer)).unwrap_or_else(|| "-".into());
                println!("  C {:<4} {:>10.4} fF  {} {}  {}", c.name, c.farads * 1e15, n.nodes[c.a].name, c.other.as_deref().map(|o| format!("to {}", o)).unwrap_or_else(|| "to ground".into()), side);
            }
        }
    }
    println!(
        "SEGMENTS: nets in the DEF {}/{} | {} wire, {} path, {} via, {} patch | R without a segment {} | C on a segment {}, without {} | check: wire segments missing an end point {}, path segments missing an end point {}, capacitor points off their segment {} | {:.2} s",
        in_def,
        sp.nets.len(),
        wire,
        path,
        via,
        patch,
        no_seg,
        caps_on,
        caps_off,
        ends_off,
        path_ends_off,
        cap_pts_off,
        t1.elapsed().as_secs_f64()
    );
    println!("  vias whose two points are apart (no wire or chain of shapes found between them): {}", via_apart);
    for (what, (n, d)) in &off_by {
        println!("  path end points off their shapes: {} {} (at most {:.1} nm away)", n, what, d);
    }
    for (w, n) in &why {
        println!("  {} R: {}", n, w);
    }
}
