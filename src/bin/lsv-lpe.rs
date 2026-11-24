//! Read post-layout (LPE) SPICE, check it against the GDS (LVS) and optionally the CDL, and attribute
//! every parasitic R, C and coupling C to a layout layer.
//! usage: lsv-lpe <main.pex.sp | cell.dspf>... --gds <cells.gds> [--layermap map] [--cdl cells.cdl] [--rules file]
//!        [--pdk NAME] [--nodes] [--no-table] [--segments] [--nets]
use lsv::cdl::{flatten_subckt, parse_cdl};
use lsv::devmatch::match_cell;
use lsv::extract::{extract_cell, parse_layer_map, ASAP7_RULES};
use lsv::gds::{flatten, parse_gds};
use lsv::lpe::{attribute_layers, compare_to_cdl, read_lpe, Src};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn usage(msg: &str) -> ! {
    eprintln!("lsv-lpe: {}", msg);
    eprintln!("usage: lsv-lpe <main.pex.sp | cell.dspf>... --gds <cells.gds> [--layermap map] [--cdl cells.cdl] [--rules file] [--pdk NAME] [--nodes] [--no-table] [--segments] [--nets]");
    std::process::exit(2);
}

fn g4(v: f64) -> String {
    if !v.is_finite() {
        return format!("{}", v);
    }
    let s = format!("{:.4e}", v);
    // short form for ordinary magnitudes
    if v != 0.0 && (v.abs() >= 1e4 || v.abs() < 1e-3) {
        s
    } else {
        let d = if v == 0.0 { 0 } else { (3 - v.abs().log10().floor() as i32).max(0) as usize };
        format!("{:.*}", d, v)
    }
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut files, mut gds, mut lmap, mut cdl_p, mut rules_p, mut show_nodes, mut table) = (vec![], None, None, None, None, false, true);
    let mut show_segments = false;
    let mut show_nets = false;
    let mut pdk: Option<&'static lsv::pdk::Preset> = None;
    let mut i = 0;
    while i < a.len() {
        let val = |i: usize| a.get(i + 1).cloned().unwrap_or_else(|| usage(&format!("{} needs a value", a[i])));
        match a[i].as_str() {
            "--gds" => {
                gds = Some(val(i));
                i += 1;
            }
            "--layermap" => {
                lmap = Some(val(i));
                i += 1;
            }
            "--cdl" => {
                cdl_p = Some(val(i));
                i += 1;
            }
            "--rules" => {
                rules_p = Some(val(i));
                i += 1;
            }
            // a built-in PDK preset (ASAP7, NG45, SKY130, ...): its layer map and extraction rules
            "--pdk" => {
                let n = val(i);
                pdk = Some(lsv::pdk::presets().iter().find(|p| p.name.eq_ignore_ascii_case(&n)).unwrap_or_else(|| usage(&format!("--pdk {}: not one of {}", n, lsv::pdk::presets().iter().map(|p| p.name).collect::<Vec<_>>().join(", ")))));
                i += 1;
            }
            "--nodes" => show_nodes = true,
            "--no-table" => table = false,
            "--segments" => show_segments = true,
            "--nets" => show_nets = true,
            x if x.starts_with('-') => usage(&format!("unknown argument: {}", x)),
            x => files.push(PathBuf::from(x)),
        }
        i += 1;
    }
    if files.is_empty() {
        usage("no LPE file given");
    }
    let gds = gds.unwrap_or_else(|| usage("--gds is required"));
    let lib = parse_gds(&lsv::io::read_all(gds.as_ref()).unwrap_or_else(|e| usage(&format!("{}: {}", gds, e))), &gds).unwrap_or_else(|e| usage(&e));
    let lm = match &lmap {
        Some(p) => parse_layer_map(&std::fs::read_to_string(p).unwrap_or_else(|e| usage(&format!("{}: {}", p, e)))),
        None => parse_layer_map(pdk.map_or(lsv::cellview::ASAP7_LAYERMAP, |p| p.layermap)),
    };
    let rules = match &rules_p {
        Some(p) => std::fs::read_to_string(p).unwrap_or_else(|e| usage(&format!("{}: {}", p, e))),
        None => pdk.map_or(ASAP7_RULES, |p| p.rules).to_string(),
    };
    let cdl = cdl_p.as_ref().map(|p| parse_cdl(&String::from_utf8_lossy(&lsv::io::read_all(p.as_ref()).unwrap_or_else(|e| usage(&format!("{}: {}", p, e)))), p));
    let mut finals = vec![];
    let mut failed = false;
    for f in &files {
        let lf = match read_lpe(f) {
            Ok(x) => x,
            Err(e) => {
                println!("ERROR {}", e);
                finals.push(format!("SUMMARY {} | ERROR {}", f.display(), e));
                failed = true;
                continue;
            }
        };
        println!("== {}", f.display());
        println!("files read: {}", lf.files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "));
        for w in &lf.warnings {
            println!("note: {}", w);
        }
        for cell in &lf.cells {
            let ncc = cell.caps.iter().filter(|c| c.coupling).count();
            let nxy = cell.nodes.iter().filter(|n| n.xy.is_some()).count();
            println!();
            println!("cell {}: devices {}, nets {}, R {}, C {}, Cc {}, nodes {} ({} with a coordinate)", cell.name, cell.subckt.mos.len(), cell.nets.len(), cell.res.len(), cell.caps.len() - ncc, ncc, cell.nodes.len(), nxy);
            println!("ports {} | nets {}", cell.subckt.ports.join(" "), cell.nets.iter().map(|n| n.name.as_str()).collect::<Vec<_>>().join(" "));
            for (al, to) in &cell.aliases {
                println!("bulk alias {} -> {}", al, to);
            }
            for w in &cell.warnings {
                println!("warning: {}", w);
            }
            for w in &cell.notes {
                println!("note: {}", w);
            }
            if show_nets {
                // per net: R count and sum, ground C count and sum, coupling C count and sum (each Cc in both its nets)
                for (k, net) in cell.nets.iter().enumerate() {
                    let rs: Vec<f64> = cell.res.iter().filter(|r| r.net == k).map(|r| r.ohms).collect();
                    let cg: Vec<f64> = cell.caps.iter().filter(|c| !c.coupling && c.net_a == k).map(|c| c.farads).collect();
                    let cc: Vec<f64> = cell.caps.iter().filter(|c| c.coupling && (c.net_a == k || c.net_b == Some(k))).map(|c| c.farads).collect();
                    println!("NET {} {} | R {} {:.6} ohm | C {} {:.8} fF | Cc {} {:.8} fF", cell.name, net.name, rs.len(), rs.iter().sum::<f64>(), cg.len(), cg.iter().sum::<f64>() * 1e15, cc.len(), cc.iter().sum::<f64>() * 1e15);
                }
            }
            // GDS + LVS
            let gname = if lib.cells.contains_key(&cell.name) { Some(cell.name.clone()) } else { lib.order.iter().find(|n| n.eq_ignore_ascii_case(&cell.name)).cloned() };
            let gname = match gname {
                Some(g) => g,
                None => {
                    println!("LVS: cell {} is not in {}", cell.name, gds);
                    finals.push(format!("SUMMARY {} | no GDS cell", cell.name));
                    failed = true;
                    continue;
                }
            };
            let flat = flatten(&lib, &gname).unwrap_or_else(|e| usage(&e));
            let ex = match extract_cell(&flat, &lm, &rules) {
                Ok(x) => x,
                Err(e) => {
                    println!("LVS: extraction failed: {}", e);
                    finals.push(format!("SUMMARY {} | extraction failed", cell.name));
                    failed = true;
                    continue;
                }
            };
            if !ex.errors.is_empty() {
                println!("extraction errors: {}", ex.errors.join("; "));
            }
            let m = match_cell(&ex, &cell.subckt, true);
            let lvs = if m.ok {
                format!("OK (groups {}, layout fingers {}, layout nets {})", m.matched_groups, ex.devices.len(), ex.nets.len())
            } else {
                format!(
                    "FAIL (groups layout {} LPE {} matched {}){}{}{}",
                    m.layout_groups,
                    m.cdl_groups,
                    m.matched_groups,
                    if m.conflicts.is_empty() { String::new() } else { format!(" | {}", m.conflicts.join("; ")) },
                    if m.unmatched_layout.is_empty() { String::new() } else { format!(" | layout only: {}", m.unmatched_layout.join("; ")) },
                    if m.unmatched_cdl.is_empty() { String::new() } else { format!(" | LPE only: {}", m.unmatched_cdl.join("; ")) }
                )
            };
            println!("LVS LPE subckt vs GDS extraction: {}", lvs);
            if !m.ok {
                failed = true;
            }
            let mut cdl_s = String::from("CDL not given");
            if let Some(c) = &cdl {
                let cn = if c.subckts.contains_key(&cell.name) { Some(cell.name.clone()) } else { c.order.iter().find(|n| n.eq_ignore_ascii_case(&cell.name)).cloned() };
                cdl_s = match cn.map(|n| flatten_subckt(c, &n)) {
                    None => {
                        failed = true;
                        format!("CDL has no subckt {}", cell.name)
                    }
                    Some(Err(e)) => {
                        failed = true;
                        format!("CDL error: {}", e)
                    }
                    Some(Ok(sc)) => {
                        let r = compare_to_cdl(&cell.subckt, &sc);
                        if r.exact.is_empty() && r.lvs_ok {
                            format!("CDL equal up to net naming, device by device ({} CDL devices, {} LPE devices incl. fingers); {}", sc.mos.len(), cell.subckt.mos.len(), r.lvs)
                        } else if r.lvs_ok {
                            format!("CDL LVS-equivalent, not equal device by device: {}; {}", r.exact.join("; "), r.lvs)
                        } else {
                            failed = true;
                            format!("CDL DIFFERS: {}; {}", r.exact.join("; "), r.lvs)
                        }
                    }
                };
                println!("{}", cdl_s);
            }
            let at = attribute_layers(cell, &ex, &m, &flat, lib.meters_per_db, &rules);
            for n in &at.notes {
                println!("note: {}", n);
            }
            // parasitics split into metal segments the way the netlist splits them (one per R, patches for C)
            let seg = lsv::lpe::segment_parasitics(cell, &at, &ex, &m, lib.meters_per_db);
            {
                use lsv::lpe::SegKind;
                let count = |k: SegKind| seg.segs.iter().filter(|s| s.kind == k).count();
                let no_seg = seg.res.iter().filter(|r| r.is_err()).count();
                let csides: usize = cell.caps.iter().map(|c| if c.coupling { 2 } else { 1 }).sum();
                let placed: usize = seg.caps.iter().zip(&cell.caps).map(|(s, c)| s[0].is_some() as usize + (c.coupling && s[1].is_some()) as usize).sum();
                let inferred = seg.inferred.iter().filter(|x| **x).count();
                let unplaced = seg.pos.iter().filter(|p| p.is_none()).count();
                // checks: a segment holds its own end points and capacitor points
                let tol = 1e-9 / lib.meters_per_db;
                let holds = |s: &lsv::lpe::Segment, p: [f64; 2]| s.rects.iter().any(|r| p[0] >= r[0] as f64 - tol && p[0] <= r[2] as f64 + tol && p[1] >= r[1] as f64 - tol && p[1] <= r[3] as f64 + tol);
                let bad_ends = seg.segs.iter().filter(|s| s.kind == SegKind::Wire && !(holds(s, s.ends[0]) && holds(s, s.ends[1]))).count();
                let off: Vec<f64> = seg
                    .caps
                    .iter()
                    .flatten()
                    .flatten()
                    .filter(|c| !holds(&seg.segs[c.seg], c.at))
                    .map(|c| seg.segs[c.seg].rects.iter().map(|r| {
                        let q = [c.at[0].clamp(r[0] as f64, r[2] as f64), c.at[1].clamp(r[1] as f64, r[3] as f64)];
                        ((q[0] - c.at[0]).powi(2) + (q[1] - c.at[1]).powi(2)).sqrt()
                    }).fold(f64::MAX, f64::min) * lib.meters_per_db * 1e9)
                    .collect();
                let bad_caps = if off.is_empty() { "0".to_string() } else { format!("{} (at most {:.1} nm away)", off.len(), off.iter().cloned().fold(0.0, f64::max)) };
                println!(
                    "SEGMENTS {} | {} segments: {} wire, {} via, {} patch | R without a segment {} | C/Cc sides on a segment {}/{} | nodes placed from R neighbours {}, without a position {} | check: wire segments missing an end point {}, capacitor points off their segment {}",
                    cell.name,
                    seg.segs.len(),
                    count(SegKind::Wire),
                    count(SegKind::Via),
                    count(SegKind::Patch),
                    no_seg,
                    placed,
                    csides,
                    inferred,
                    unplaced,
                    bad_ends,
                    bad_caps
                );
                if show_segments {
                    for (i, s) in seg.segs.iter().enumerate() {
                        let (len, w) = lsv::lpe::seg_size_um(s, lib.meters_per_db);
                        let r = s.res.map(|k| format!("{:.3} ohm", cell.res[k].ohms)).unwrap_or_else(|| "-".into());
                        let (mut cg, mut cc, mut ncc) = (0.0, 0.0, 0);
                        for (k, c) in cell.caps.iter().enumerate() {
                            for side in seg.caps[k].iter().flatten() {
                                if side.seg == i {
                                    if c.coupling {
                                        cc += c.farads * 0.5;
                                        ncc += 1;
                                    } else {
                                        cg += c.farads;
                                    }
                                }
                            }
                        }
                        let ends = format!("{} - {}", cell.nodes[s.nodes[0]].name, cell.nodes[s.nodes[1]].name);
                        println!("  {:?} {:<28} {:.4} um long {:.4} um wide  R {:>12}  Cgnd {:.4} fF  Cc {} sides (half each) {:.4} fF  nodes {}", s.kind, lsv::lpe::seg_label(cell, &seg, i), len, w, r, cg * 1e15, ncc, cc * 1e15, ends);
                    }
                    for (k, r) in seg.res.iter().enumerate() {
                        if let Err(why) = r {
                            println!("  R {} has no segment: {}", cell.res[k].name, why);
                        }
                    }
                    for (i, p) in seg.pos.iter().enumerate() {
                        if let (true, Some(p)) = (seg.inferred[i], p) {
                            println!("  node {} placed at ({:.4}, {:.4}) um", cell.nodes[i].name, p[0] * lib.meters_per_db * 1e6, p[1] * lib.meters_per_db * 1e6);
                        }
                    }
                    for (k, sides) in seg.caps.iter().enumerate() {
                        for (s, side) in sides.iter().enumerate() {
                            if let Some(c) = side {
                                if !holds(&seg.segs[c.seg], c.at) {
                                    println!("  C {} side {} at ({:.4}, {:.4}) um is off its segment {}", cell.caps[k].name, if s == 0 { "A" } else { "B" }, c.at[0] * lib.meters_per_db * 1e6, c.at[1] * lib.meters_per_db * 1e6, lsv::lpe::seg_label(cell, &seg, c.seg));
                                }
                            }
                        }
                    }
                }
            }
            let netn = |k: usize| cell.nets[k].name.as_str();
            let nodn = |k: usize| cell.nodes[k].name.as_str();
            let lay = |l: &Option<String>| l.clone().unwrap_or_else(|| "unknown".into());
            // a layer that rests on a stack pick (top layer of an unresolved via/contact stack)
            let mark = |l: &Option<String>, node: usize| if l.is_some() && at.nodes[node].src == Src::Stack { format!("{} [stack pick: {}]", lay(l), at.nodes[node].candidates.join("+")) } else { lay(l) };
            if show_nodes {
                println!("nodes: name | net | xy | from | candidates | cuts | layer | source | how");
                for (k, nd) in cell.nodes.iter().enumerate() {
                    let o = &at.nodes[k];
                    println!(
                        "  N {} | {} | {} | {} | {} | {} | {} | {:?} | {}",
                        nd.name,
                        netn(nd.net),
                        o.xy.map(|p| format!("({}, {})", p[0], p[1])).unwrap_or_else(|| "-".into()),
                        if o.xy_from.is_empty() { "-" } else { &o.xy_from },
                        o.candidates.join("+"),
                        o.cuts.join("+"),
                        lay(&o.layer),
                        o.src,
                        o.how
                    );
                }
            }
            if table {
                println!("elements: kind | name | value | net(s) | layer(s) | nodes | how");
                for (r, o) in cell.res.iter().zip(&at.res) {
                    let pick = if o.by_value { " [value match]" } else if o.layer.is_some() && o.ends_src.contains(&Src::Stack) { " [stack pick]" } else { "" };
                    println!("  R  | {} | {} ohm | {} | {}{} | {} {} | {}", r.name, g4(r.ohms), netn(r.net), lay(&o.layer), pick, nodn(r.a), nodn(r.b), o.how);
                }
                for (c, o) in cell.caps.iter().zip(&at.caps).filter(|(c, _)| !c.coupling) {
                    println!("  C  | {} | {} fF | {} | {} | {} | {}", c.name, g4(c.farads * 1e15), netn(c.net_a), mark(&o.a, c.a), nodn(c.a), o.how);
                }
                for (c, o) in cell.caps.iter().zip(&at.caps).filter(|(c, _)| c.coupling) {
                    let b = c.b.unwrap();
                    println!("  Cc | {} | {} fF | {} / {} | {} / {} | {} {} | {}", c.name, g4(c.farads * 1e15), netn(c.net_a), netn(c.net_b.unwrap()), mark(&o.a, c.a), mark(&o.b, b), nodn(c.a), nodn(b), o.how);
                }
            }
            // summary
            let r_ok = at.res.iter().filter(|o| o.layer.is_some()).count();
            let c_all: Vec<_> = cell.caps.iter().zip(&at.caps).filter(|(c, _)| !c.coupling).collect();
            let cc_all: Vec<_> = cell.caps.iter().zip(&at.caps).filter(|(c, _)| c.coupling).collect();
            let c_ok = c_all.iter().filter(|(_, o)| o.a.is_some()).count();
            let cc_ok = cc_all.iter().filter(|(_, o)| o.a.is_some() && o.b.is_some()).count();
            let cc_sides = cc_all.iter().map(|(_, o)| o.a.is_some() as usize + o.b.is_some() as usize).sum::<usize>();
            println!("summary {}:", cell.name);
            println!("  R : {} total, {} attributed, {} unknown", cell.res.len(), r_ok, cell.res.len() - r_ok);
            println!("  C : {} total, {} attributed, {} unknown", c_all.len(), c_ok, c_all.len() - c_ok);
            println!("  Cc: {} total, {} attributed on both sides, {} with an unknown side ({} of {} sides attributed)", cc_all.len(), cc_ok, cc_all.len() - cc_ok, cc_sides, 2 * cc_all.len());
            let mut by_r: BTreeMap<String, (usize, f64)> = BTreeMap::new();
            for (r, o) in cell.res.iter().zip(&at.res) {
                let e = by_r.entry(lay(&o.layer)).or_insert((0, 0.0));
                e.0 += 1;
                e.1 += r.ohms;
            }
            let mut by_c: BTreeMap<String, (usize, f64)> = BTreeMap::new();
            for (c, o) in &c_all {
                let e = by_c.entry(lay(&o.a)).or_insert((0, 0.0));
                e.0 += 1;
                e.1 += c.farads * 1e15;
            }
            let mut by_cc: BTreeMap<String, (usize, f64)> = BTreeMap::new();
            for (c, o) in &cc_all {
                for l in [&o.a, &o.b] {
                    let e = by_cc.entry(lay(l)).or_insert((0, 0.0));
                    e.0 += 1;
                    e.1 += c.farads * 1e15;
                }
            }
            let fmt = |m: &BTreeMap<String, (usize, f64)>, unit: &str| m.iter().map(|(k, v)| format!("{} {} ({} {})", k, v.0, g4(v.1), unit)).collect::<Vec<_>>().join(", ");
            println!("  R by layer : {}", fmt(&by_r, "ohm"));
            println!("  C by layer : {}", fmt(&by_c, "fF"));
            println!("  Cc by side layer (each side counted, value of the cap): {}", fmt(&by_cc, "fF"));
            let mut srcs: BTreeMap<String, usize> = BTreeMap::new();
            for o in &at.nodes {
                *srcs.entry(format!("{:?}", o.src)).or_insert(0) += 1;
            }
            println!("  node layer sources: {}", srcs.iter().map(|(k, v)| format!("{} {}", k, v)).collect::<Vec<_>>().join(", "));
            let unknown_r: Vec<String> = cell.res.iter().zip(&at.res).filter(|(_, o)| o.layer.is_none()).map(|(r, o)| format!("{} ({})", r.name, o.how)).collect();
            if !unknown_r.is_empty() {
                println!("  unknown R: {}", unknown_r.join("; "));
            }
            let stacks = at.nodes.iter().filter(|o| o.src == Src::Stack).count();
            let on_stack = |n: usize| at.nodes[n].src == Src::Stack;
            let c_stack = c_all.iter().filter(|(c, o)| o.a.is_some() && on_stack(c.a) && c.tag_a.is_none()).count();
            let cc_stack = cc_all.iter().filter(|(c, o)| o.a.is_some() && o.b.is_some() && (on_stack(c.a) || on_stack(c.b.unwrap()))).count();
            let r_stack = at.res.iter().filter(|o| o.layer.is_some() && !o.by_value && o.ends_src.contains(&Src::Stack)).count();
            let r_value = at.res.iter().filter(|o| o.by_value).count();
            println!("  R attributed by value (equal to R placed by position in this cell): {}", r_value);
            println!("  attributed through a stack pick (top layer of an unresolved via/contact stack): R {}, C {}, Cc {} ({} stack nodes)", r_stack, c_stack, cc_stack, stacks);
            let tot_c = c_all.len() + cc_all.len();
            let ok_c = c_ok + cc_ok;
            finals.push(format!(
                "SUMMARY {} | LVS {} | {} | R {}/{} attributed | C {}/{} | Cc {}/{} (both sides) | C+Cc {:.1}% attributed, {:.1}% without a stack pick | stack picks {} nodes | R by value {}",
                cell.name,
                if m.ok { "OK" } else { "FAIL" },
                if cdl.is_none() { "CDL not given".to_string() } else if cdl_s.starts_with("CDL equal") { "CDL equal".to_string() } else if cdl_s.starts_with("CDL LVS-equivalent") { "CDL LVS-equivalent (series order differs)".to_string() } else { "CDL DIFFERS".to_string() },
                r_ok,
                cell.res.len(),
                c_ok,
                c_all.len(),
                cc_ok,
                cc_all.len(),
                if tot_c == 0 { 100.0 } else { 100.0 * ok_c as f64 / tot_c as f64 },
                if tot_c == 0 { 100.0 } else { 100.0 * (ok_c - c_stack - cc_stack) as f64 / tot_c as f64 },
                stacks,
                r_value
            ));
        }
    }
    println!();
    for s in &finals {
        println!("{}", s);
    }
    if failed {
        std::process::exit(1);
    }
}
