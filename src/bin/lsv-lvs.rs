//! Extract every cell of a GDS and match it to the CDL; one line per cell and a summary.
//! usage: lsv-lvs <gds> <cdl> [layermap, the preset's map when omitted] [--pdk NAME|auto|FILE.lsvpdk] [--rules FILE]
//!        [--only NAME-substring] [--dump] [--mutate] [--detect]
//! --pdk auto (default) picks a built-in preset, or one from a *.lsvpdk file in LSV_PDK_PATH, from the GDS
//! layer/datatype pairs; the chosen preset is reported on stderr so stdout stays the per-cell report.
//! --detect prints the preset scores for the GDS and exits (the CDL argument may then be omitted).
use lsv::cdl::{flatten_subckt, parse_cdl, Subckt};
use lsv::devmatch::{cdl_l, cdl_w, match_cell};
use lsv::extract::{extract_cell, parse_layer_map};
use lsv::gds::{flatten, parse_gds};
use lsv::pdk;
use std::collections::{BTreeMap, HashMap, HashSet};

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut pos, mut rules_p, mut only, mut dump, mut mutate) = (vec![], None, None, false, false);
    let (mut pdk_arg, mut detect_only) = ("auto".to_string(), false);
    let mut i = 0;
    while i < a.len() {
        match a[i].as_str() {
            "--rules" => { rules_p = a.get(i + 1).cloned(); i += 1; }
            "--only" => { only = a.get(i + 1).cloned(); i += 1; }
            "--pdk" => { pdk_arg = a.get(i + 1).cloned().unwrap_or_default(); i += 1; }
            "--dump" => dump = true,
            "--mutate" => mutate = true,
            "--detect" => detect_only = true,
            x => pos.push(x.to_string()),
        }
        i += 1;
    }
    if pos.is_empty() || (!detect_only && pos.len() < 2) {
        eprintln!("usage: lsv-lvs <gds> <cdl> [layermap] [--pdk NAME|auto|FILE.lsvpdk] [--rules FILE] [--only NAME] [--dump] [--mutate]");
        eprintln!("       lsv-lvs <gds> --detect");
        std::process::exit(2);
    }
    let t0 = std::time::Instant::now();
    let lib = parse_gds(&lsv::io::read_all(std::path::Path::new(&pos[0])).expect("gds"), &pos[0]).expect("gds parse");
    let (mut list, errs) = pdk::all_presets();
    for e in errs {
        eprintln!("warning: {}", e);
    }
    let used = pdk::layers_used(&lib);
    if detect_only {
        let mut sc: Vec<(f64, &str)> = list.iter().map(|p| (pdk::score(p, &used), p.name)).collect();
        sc.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        let pick = pdk::detect_among(&list, &used).map(|k| list[k].name).unwrap_or("none");
        println!("{}: {} layer/datatype pairs, detected {} | {}", pos[0], used.len(), pick, sc.iter().map(|(s, n)| format!("{} {:.3}", n, s)).collect::<Vec<_>>().join(", "));
        return;
    }
    let k = if pdk_arg.eq_ignore_ascii_case("auto") {
        match pdk::detect_among(&list, &used) {
            Some(k) => {
                eprintln!("pdk: {} (auto-detected from {} layer/datatype pairs)", list[k].name, used.len());
                k
            }
            None => {
                eprintln!("pdk: no preset matches the layers of {}; give --pdk NAME (built in: {})", pos[0], list.iter().map(|p| p.name).collect::<Vec<_>>().join(", "));
                std::process::exit(2);
            }
        }
    } else if let Some(k) = pdk::find(&list, &pdk_arg) {
        eprintln!("pdk: {}", list[k].name);
        k
    } else if std::path::Path::new(&pdk_arg).is_file() {
        match pdk::load_file(std::path::Path::new(&pdk_arg)) {
            Ok(p) => {
                eprintln!("pdk: {} (from {})", p.name, pdk_arg);
                list.push(p);
                list.len() - 1
            }
            Err(e) => {
                eprintln!("pdk: {}", e);
                std::process::exit(2);
            }
        }
    } else {
        eprintln!("pdk: unknown preset {} (known: {}, or a .lsvpdk file)", pdk_arg, list.iter().map(|p| p.name).collect::<Vec<_>>().join(", "));
        std::process::exit(2);
    };
    let preset = list[k];
    let cdl = parse_cdl(&String::from_utf8_lossy(&lsv::io::read_all(std::path::Path::new(&pos[1])).expect("cdl")), &pos[1]);
    if !cdl.warnings.is_empty() {
        eprintln!("cdl: {} reader warnings{}", cdl.warnings.len(), if cdl.warnings.len() >= 100 { " (first 100 kept)" } else { "" });
        for w in cdl.warnings.iter().take(20) {
            eprintln!("  {}", w);
        }
    }
    let lm = match pos.get(2) {
        Some(p) => parse_layer_map(&std::fs::read_to_string(p).expect("layermap")),
        None => parse_layer_map(preset.layermap),
    };
    let rules = rules_p.map(|p| std::fs::read_to_string(p).expect("rules")).unwrap_or_else(|| preset.rules.to_string());
    if mutate {
        return mutation(&lib, &cdl, &lm, &rules);
    }
    let mut non_mos: Vec<String> = vec![];
    // OK cells whose pin texts and CDL ports disagree by name (texts naming no CDL net, ports with no text)
    let mut pin_names: Vec<String> = vec![];
    let (mut ok, mut bad, mut no_cdl, mut fin_only, mut cells) = (0, 0, 0, 0, 0);
    let mut warn_classes: BTreeMap<String, usize> = BTreeMap::new();
    let (mut t_dev, mut t_nofinger, mut t_gate, mut t_fin, mut t_fingers, mut t_orphan, mut t_nets) = (0, 0, 0, 0, 0, 0, 0);
    let mut net_missing: Vec<String> = vec![];
    // OK cells with interchangeable copies, paired by order
    let mut sym_cells: Vec<String> = vec![];
    for name in &lib.order {
        if let Some(o) = &only {
            if !name.contains(o.as_str()) {
                continue;
            }
        }
        cells += 1;
        if !cdl.subckts.contains_key(name) {
            no_cdl += 1;
            println!("NOCDL  {}", name);
            continue;
        }
        let flat = flatten(&lib, name).expect("flatten");
        let ex = extract_cell(&flat, &lm, &rules).expect("extract");
        for w in &ex.warnings {
            *warn_classes.entry(warn_class(w)).or_insert(0) += 1;
        }
        let sc = flatten_subckt(&cdl, name).expect("subckt");
        let m = match_cell(&ex, &sc, true);
        let nd = sc.mos.len();
        if m.ok {
            let mut owner: HashMap<usize, HashSet<String>> = HashMap::new();
            for d in &sc.mos {
                let fs = m.dev_to_layout.get(&d.name).cloned().unwrap_or_default();
                if fs.is_empty() {
                    t_nofinger += 1;
                }
                let mut fins = 0.0;
                for &f in &fs {
                    fins += ex.devices[f].nfin;
                    if m.net_map.get(&(ex.devices[f].gate.max(0) as usize)).map(|s| s.as_str()) != Some(d.g.as_str()) || ex.devices[f].gate < 0 {
                        t_gate += 1;
                    }
                    owner.entry(f).or_default().insert(d.name.clone());
                }
                let sharing = sc.mos.iter().filter(|x| m.dev_to_layout.get(&x.name).map_or(false, |v| v.iter().any(|f| fs.contains(f))));
                if ex.size_wl {
                    // planar: summed layout width and the length of every finger against the CDL
                    let want: f64 = sharing.map(|x| cdl_w(x, ex.cdl_scale)).sum();
                    let have: f64 = fs.iter().map(|&f| ex.devices[f].w).sum();
                    let l = cdl_l(d, ex.cdl_scale);
                    if (have - want).abs() > 5e-4 * want.abs().max(have.abs()) || fs.iter().any(|&f| (ex.devices[f].l - l).abs() > 5e-4 * l.abs()) {
                        t_fin += 1;
                    }
                } else {
                    let want: f64 = sharing.map(|x| x.nfin).sum();
                    if fins != want {
                        t_fin += 1;
                    }
                }
                t_dev += 1;
            }
            t_fingers += ex.devices.len();
            t_orphan += (0..ex.devices.len()).filter(|f| !owner.contains_key(f)).count();
            let mut seen = HashSet::new();
            for d in &sc.mos {
                for nn in [&d.d, &d.g, &d.s] {
                    if !seen.insert(nn.clone()) {
                        continue;
                    }
                    if m.net_to_layout.get(nn).map_or(false, |v| !v.is_empty()) {
                        t_nets += 1;
                    } else {
                        net_missing.push(format!("{}:{}", name, nn));
                    }
                }
            }
            if dump {
                dump_cell(name, &sc, &ex, &m, flat.dbu_m);
            }
            ok += 1;
            let cdl_nets: HashSet<&str> = sc.ports.iter().map(|s| s.as_str()).chain(sc.mos.iter().flat_map(|d| [d.d.as_str(), d.g.as_str(), d.s.as_str(), d.b.as_str()])).collect();
            let texts: HashSet<&str> = ex.nets.iter().flat_map(|n| n.names.iter().map(|s| s.as_str())).collect();
            let mut stray: Vec<&str> = texts.iter().copied().filter(|t| !cdl_nets.contains(t)).collect();
            // ports on a gate, source or drain; a port used only as bulk (a well or substrate) may be untexted
            let used: HashSet<&str> = sc.mos.iter().flat_map(|d| [d.d.as_str(), d.g.as_str(), d.s.as_str()]).collect();
            let mut bare: Vec<&str> = sc.ports.iter().map(|s| s.as_str()).filter(|p| used.contains(p) && !texts.contains(p)).collect();
            if !stray.is_empty() || !bare.is_empty() {
                stray.sort();
                bare.sort();
                pin_names.push(format!("{} (texts {} | ports {})", name, stray.join(","), bare.join(",")));
            }
            let other = cdl.subckts.get(name).map_or(0, |s| s.other);
            if other > 0 {
                non_mos.push(format!("{} ({})", name, other));
            }
            if !m.symmetric_devs.is_empty() {
                sym_cells.push(format!("{} ({})", name, m.symmetric_devs.len()));
            }
            println!(
                "OK     {}  devices {} cdl / {} layout fingers, groups {}, nets {}, warnings {}{}{}",
                name,
                nd,
                ex.devices.len(),
                m.matched_groups,
                ex.nets.len(),
                ex.warnings.len(),
                if other > 0 { format!(", {} CDL R/C/D elements not compared", other) } else { String::new() },
                if m.symmetric_devs.is_empty() { String::new() } else { format!(", symmetric: {} devices have interchangeable layout copies, paired by order (CDL first use to layout left to right){}", m.symmetric_devs.len(), if m.symmetric_nets.is_empty() { String::new() } else { format!(", nets {}", m.symmetric_nets.join(" ")) }) }
            );
            if dump && !m.symmetric_devs.is_empty() {
                println!("SYM {} devices {} | nets {}", name, m.symmetric_devs.join(" "), m.symmetric_nets.join(" "));
            }
            continue;
        }
        let m2 = match_cell(&ex, &sc, false);
        if m2.ok {
            fin_only += 1;
        }
        if dump {
            dump_fail(name, &sc, &ex);
        }
        bad += 1;
        println!(
            "FAIL   {}  groups layout {} cdl {} matched {}{} | {} | errors {} | warnings {}: {} | search steps {}{} | unmatched layout: {} | unmatched cdl: {}",
            name,
            m.layout_groups,
            m.cdl_groups,
            m.matched_groups,
            if m2.ok { " (matches if fins are ignored)" } else { "" },
            m.conflicts.join("; "),
            ex.errors.join("; "),
            ex.warnings.len(),
            ex.warnings.iter().take(3).cloned().collect::<Vec<_>>().join("; "),
            m.steps,
            if m.steps > 300_000 { " (search limit hit)" } else { "" },
            m.unmatched_layout.iter().take(4).cloned().collect::<Vec<_>>().join("; "),
            m.unmatched_cdl.iter().take(4).cloned().collect::<Vec<_>>().join("; ")
        );
        if m.causes.is_empty() {
            println!("       cause: none found by the pin-text and CDL-line checks, compare the unmatched groups above");
        }
        for c in &m.causes {
            println!("       cause: {}", c);
        }
    }
    println!();
    println!("warning classes over all cells:");
    let mut wc: Vec<_> = warn_classes.into_iter().collect();
    wc.sort_by(|a, b| b.1.cmp(&a.1));
    for (k, v) in wc {
        println!("  {:>5}  {}", v, k);
    }
    println!(
        "device trace: {} CDL devices, {} without fingers, {} finger gate mismatches, {} fin-sum mismatches | {} layout fingers, {} without a CDL device | CDL nets with layout shapes {}, without {}{}",
        t_dev,
        t_nofinger,
        t_gate,
        t_fin,
        t_fingers,
        t_orphan,
        t_nets,
        net_missing.len(),
        if net_missing.is_empty() { String::new() } else { format!(": {}", net_missing.iter().take(12).cloned().collect::<Vec<_>>().join(" ")) }
    );
    if !pin_names.is_empty() {
        println!(
            "OK cells whose pin texts name no CDL net, or whose device-connected CDL ports carry no text: {}: {}",
            pin_names.len(),
            pin_names.iter().take(30).cloned().collect::<Vec<_>>().join(" ")
        );
    }
    if !non_mos.is_empty() {
        println!("OK cells whose CDL also has R/C/D elements, which are not extracted or compared: {}: {}", non_mos.len(), non_mos.join(" "));
    }
    if !sym_cells.is_empty() {
        println!("OK cells with symmetric copies (several device pairings pass, the one shown is picked by order): {}: {}", sym_cells.len(), sym_cells.iter().take(40).cloned().collect::<Vec<_>>().join(" "));
    }
    println!("cells {}: OK {}, FAIL {} (of which {} match when fins are ignored), no CDL {}, {:.1} s", cells, ok, bad, fin_only, no_cdl, t0.elapsed().as_secs_f64());
}

fn warn_class(w: &str) -> String {
    let mut out = String::new();
    let (mut in_q, mut in_p) = (false, false);
    let mut last_digit = false;
    for c in w.chars() {
        if in_q {
            if c == '"' {
                in_q = false;
                out.push_str("X\"");
            }
            continue;
        }
        if in_p {
            if c == ')' {
                in_p = false;
                out.push_str("x,y)");
            }
            continue;
        }
        if c == '"' {
            in_q = true;
            out.push('"');
            last_digit = false;
            continue;
        }
        if c == '(' {
            in_p = true;
            out.push('(');
            last_digit = false;
            continue;
        }
        if c.is_ascii_digit() {
            if !last_digit {
                out.push('N');
            }
            last_digit = true;
            continue;
        }
        last_digit = false;
        out.push(c);
    }
    out
}

fn dump_cell(name: &str, sc: &Subckt, ex: &lsv::extract::Extraction, m: &lsv::devmatch::MatchResult, dbu_m: f64) {
    for d in &sc.mos {
        let mut fs = m.dev_to_layout.get(&d.name).cloned().unwrap_or_default();
        fs.sort();
        fs.dedup();
        let fins: f64 = fs.iter().map(|&f| ex.devices[f].nfin).sum();
        // channel lower-left corner of each finger, um
        let at: Vec<String> = fs.iter().filter(|&&f| !ex.devices[f].rects.is_empty()).map(|&f| {
            let rs = &ex.devices[f].rects;
            let (x, y) = (rs.iter().map(|r| r[0]).min().unwrap(), rs.iter().map(|r| r[1]).min().unwrap());
            format!("{:.4},{:.4}", x as f64 * dbu_m * 1e6, y as f64 * dbu_m * 1e6)
        }).collect();
        println!("DEV {} {} fingers {:?} fins {} at {}", name, d.name, fs, fins, at.join(" "));
    }
    let mut nets: Vec<_> = m.net_to_layout.iter().collect();
    nets.sort();
    for (n, v) in nets {
        let mut v = v.clone();
        v.sort();
        println!("NET {} {} {:?}", name, n, v);
    }
}

/// a failing cell: every layout finger (nets by label or id, size) and every CDL device, for comparison by eye
fn dump_fail(name: &str, sc: &Subckt, ex: &lsv::extract::Extraction) {
    let nn = |id: i64| -> String {
        if id < 0 {
            return "?".into();
        }
        ex.nets.get(id as usize).and_then(|n| n.name.clone()).unwrap_or_else(|| format!("#{}", id))
    };
    for (i, d) in ex.devices.iter().enumerate() {
        let sd: Vec<String> = d.sd.iter().map(|&x| nn(x as i64)).collect();
        let at = d.rects.first().map(|r| format!("({}, {})", r[0], r[1])).unwrap_or_default();
        println!("LAYOUT {} f{} {} g={} sd={} b={} w={:.4}u l={:.4}u nfin={} at {}", name, i, d.typ, nn(d.gate), sd.join(","), nn(d.bulk), d.w * 1e6, d.l * 1e6, d.nfin, at);
    }
    for d in &sc.mos {
        println!("CDL    {} {} {} d={} g={} s={} b={} w={:.4}u l={:.4}u nfin={} m={}", name, d.name, d.typ, d.d, d.g, d.s, d.b, cdl_w(d, ex.cdl_scale) * 1e6, cdl_l(d, ex.cdl_scale) * 1e6, d.nfin, d.m);
    }
    for n in &ex.nets {
        let on_dev = ex.devices.iter().filter(|d| d.gate == n.id as i64 || d.sd.contains(&n.id)).count();
        if !n.names.is_empty() {
            println!("NET    {} #{} labels {} layers {} devices {}", name, n.id, n.names.join(","), n.layers.join(","), on_dev);
        }
    }
}

fn mutation(lib: &lsv::gds::GdsLib, cdl: &lsv::cdl::Cdl, lm: &lsv::extract::LayerMap, rules: &str) {
    let mut stats = [(0usize, 0usize); 3];
    let mut survivors = vec![];
    let mut size_name: &str = "fins";
    for name in &lib.order {
        if !cdl.subckts.contains_key(name) {
            continue;
        }
        let ex = extract_cell(&flatten(lib, name).unwrap(), lm, rules).unwrap();
        let sc = flatten_subckt(cdl, name).unwrap();
        if ex.size_wl {
            size_name = "width";
        }
        if !match_cell(&ex, &sc, true).ok {
            println!("baseline FAIL {}", name);
            continue;
        }
        let mut nets: Vec<String> = vec![];
        for d in &sc.mos {
            for x in [&d.d, &d.g, &d.s] {
                if !nets.contains(x) {
                    nets.push(x.clone());
                }
            }
        }
        for (i, d) in sc.mos.iter().enumerate() {
            let other = nets.iter().find(|n| **n != d.g && **n != d.d && **n != d.s).cloned();
            for kind in 0..3 {
                let mut nd = d.clone();
                match kind {
                    0 => match &other { Some(o) => nd.g = o.clone(), None => continue },
                    1 if ex.size_wl => nd.w *= 1.25,
                    1 => nd.nfin += 1.0,
                    _ => match &other { Some(o) => nd.d = o.clone(), None => continue },
                }
                let mut sc2 = sc.clone();
                sc2.mos[i] = nd.clone();
                stats[kind].0 += 1;
                if match_cell(&ex, &sc2, true).ok {
                    stats[kind].1 += 1;
                    survivors.push(format!("{} {} {}", name, ["gate", size_name, "drain"][kind], d.name));
                }
            }
        }
    }
    for (k, (n, s)) in ["gate", size_name, "drain"].iter().zip(stats) {
        println!("{:<6} mutants {}, still matched {}", k, n, s);
    }
    for s in survivors.iter().take(40) {
        println!("  survived: {}", s);
    }
}
