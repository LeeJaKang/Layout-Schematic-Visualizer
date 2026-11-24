//! Logic check of library cells: the transistors of each cell (CDL or post-layout netlist, and the GDS
//! extraction when a GDS is given) are simulated for every input vector and compared with the Liberty functions.
//! usage: lsv-func <cells.cdl|post-layout.sp> --lib <cells.lib>... [--gds <cells.gds> [--pdk NAME|auto|FILE.lsvpdk]
//!        [--rules FILE] [--layermap FILE]] [--only NAME-substring] [--func "Y = !(A & B)"] [--negate] [--rows]
//! --func checks the cells picked by --only against the given function instead of the Liberty ones.
//! --negate checks the inverse of every function, so every checked cell must come out FAIL (a control).
//! --trace prints the level of every node at the first vector that differs.
use lsv::cdl::{flatten_subckt, parse_cdl, Cdl};
use lsv::devmatch::match_cell;
use lsv::extract::{extract_cell, parse_layer_map};
use lsv::func::{level_char, run, Circuit};
use lsv::gds::{flatten, parse_gds};
use lsv::liberty::parse_liberty;
use lsv::pdk;
use std::collections::HashMap;
use std::io::Write;

/// print a line, ending quietly when the reader has gone (output piped into head)
macro_rules! out {
    ($($t:tt)*) => {
        if writeln!(std::io::stdout(), $($t)*).is_err() {
            std::process::exit(0);
        }
    };
}

fn usage() -> ! {
    eprintln!("usage: lsv-func <cells.cdl|post-layout.sp> --lib <cells.lib>... [--gds <cells.gds> [--pdk NAME|auto|FILE.lsvpdk] [--rules FILE] [--layermap FILE]]");
    eprintln!("                [--only NAME] [--func \"Y = !(A & B)\"] [--negate] [--rows] [--trace]");
    std::process::exit(2);
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let (mut pos, mut libs, mut gds_p, mut rules_p, mut lmap_p, mut only, mut func) = (vec![], vec![], None, None, None, None, None);
    let (mut pdk_arg, mut negate, mut rows, mut trace) = ("auto".to_string(), false, false, false);
    let mut i = 0;
    while i < a.len() {
        let next = |i: &mut usize| -> String {
            *i += 1;
            a.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match a[i].as_str() {
            "--lib" => libs.push(next(&mut i)),
            "--gds" => gds_p = Some(next(&mut i)),
            "--pdk" => pdk_arg = next(&mut i),
            "--rules" => rules_p = Some(next(&mut i)),
            "--layermap" => lmap_p = Some(next(&mut i)),
            "--only" => only = Some(next(&mut i)),
            "--func" => func = Some(next(&mut i)),
            "--negate" => negate = true,
            "--rows" => rows = true,
            "--trace" => trace = true,
            x if x.starts_with("--") => usage(),
            x => pos.push(x.to_string()),
        }
        i += 1;
    }
    if pos.len() != 1 || (libs.is_empty() && func.is_none()) {
        usage();
    }
    let t0 = std::time::Instant::now();
    let text = String::from_utf8_lossy(&lsv::io::read_all(std::path::Path::new(&pos[0])).expect("netlist")).into_owned();
    let cdl = if lsv::cellview::looks_like_lpe(&text) {
        let f = lsv::lpe::read_lpe(std::path::Path::new(&pos[0])).expect("post-layout netlist");
        eprintln!("netlist: post-layout, {} cells", f.cells.len());
        let mut c = Cdl::default();
        for cell in f.cells {
            c.order.push(cell.name.clone());
            c.subckts.insert(cell.name.clone(), cell.subckt);
        }
        c
    } else {
        parse_cdl(&text, &pos[0])
    };
    // functions by cell, or why the cell has none
    let mut funcs: HashMap<String, Result<String, String>> = HashMap::new();
    for l in &libs {
        let b = lsv::io::read_all(std::path::Path::new(l)).expect("liberty");
        for c in parse_liberty(&b, l).expect("liberty parse") {
            funcs.entry(c.name.clone()).or_insert_with(|| lsv::func::liberty_text(&c));
        }
    }
    // layout: the preset picked from the GDS layers, as lsv-lvs does
    let layout = gds_p.map(|p| {
        let lib = parse_gds(&lsv::io::read_all(std::path::Path::new(&p)).expect("gds"), &p).expect("gds parse");
        let (mut list, _) = pdk::all_presets();
        let used = pdk::layers_used(&lib);
        let k = if pdk_arg.eq_ignore_ascii_case("auto") {
            pdk::detect_among(&list, &used).unwrap_or_else(|| {
                eprintln!("pdk: no preset matches the layers of {}, give --pdk", p);
                std::process::exit(2)
            })
        } else if let Some(k) = pdk::find(&list, &pdk_arg) {
            k
        } else {
            list.push(pdk::load_file(std::path::Path::new(&pdk_arg)).unwrap_or_else(|e| {
                eprintln!("pdk: {}", e);
                std::process::exit(2)
            }));
            list.len() - 1
        };
        eprintln!("pdk: {}", list[k].name);
        let lm = parse_layer_map(&lmap_p.as_ref().map(|f| std::fs::read_to_string(f).expect("layermap")).unwrap_or_else(|| list[k].layermap.to_string()));
        let rules = rules_p.as_ref().map(|f| std::fs::read_to_string(f).expect("rules")).unwrap_or_else(|| list[k].rules.to_string());
        (lib, lm, rules)
    });
    let (mut ok, mut bad, mut seq, mut nofunc, mut nolib, mut vectors) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut errors, mut no_layout) = (0usize, 0usize);
    let mut fails: Vec<String> = vec![];
    for name in &cdl.order {
        if only.as_ref().map_or(false, |o| !name.contains(o.as_str())) {
            continue;
        }
        let sc = match flatten_subckt(&cdl, name) {
            Ok(s) if !s.mos.is_empty() => s,
            _ => continue,
        };
        let text = match func.clone().map(Ok).or_else(|| funcs.get(name).cloned()) {
            None => {
                nolib += 1;
                out!("NOLIB  {}", name);
                continue;
            }
            Some(Err(why)) => {
                if why.starts_with("sequential") || why.starts_with("holds a state") {
                    seq += 1;
                } else {
                    nofunc += 1;
                }
                out!("SKIP   {}: {}", name, why);
                continue;
            }
            Some(Ok(t)) => t,
        };
        let text = if negate {
            // one spec per line, comments dropped, so ';' and '#' in --func negate like the Liberty texts
            let flat: Vec<String> = text.lines().flat_map(|l| l.split('#').next().unwrap_or("").split(';').map(|s| s.trim().to_string()).collect::<Vec<_>>()).filter(|s| !s.is_empty()).collect();
            flat.iter()
                .map(|l| match l.split_once('=') {
                    Some((o, f)) if !o.trim().to_ascii_lowercase().starts_with("three_state") => format!("{} = !({})", o.trim(), f.trim().trim_matches('"')),
                    _ => l.to_string(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            text
        };
        let mut circuits: Vec<(String, Circuit)> = vec![("schematic".into(), Circuit::from_subckt(&sc))];
        let mut note = String::new();
        if let Some((lib, lm, rules)) = &layout {
            if lib.cells.contains_key(name) {
                let flat = flatten(lib, name).expect("flatten");
                match extract_cell(&flat, lm, rules) {
                    // an extraction with rule errors is incomplete: the layout is not checked, as in the viewer
                    Ok(ex) if !ex.errors.is_empty() => note = format!(" (extraction rules: {} errors, layout not checked)", ex.errors.len()),
                    Ok(ex) => {
                        let m = match_cell(&ex, &sc, true);
                        if !m.ok {
                            note = " (LVS does not match)".into();
                        }
                        circuits.push(("layout".into(), Circuit::from_extraction(&ex, Some(&m))));
                    }
                    Err(e) => note = format!(" (extraction: {}, layout not checked)", e),
                }
            } else {
                note = " (not in the GDS, layout not checked)".into();
            }
            if circuits.len() == 1 {
                no_layout += 1;
            }
        }
        let refs: Vec<(String, &Circuit)> = circuits.iter().map(|(l, c)| (l.clone(), c)).collect();
        let o = match run(&text, &refs) {
            Ok(o) => o,
            Err(e) => {
                // the functions cannot be checked at all (their text, a stored state, too many inputs)
                errors += 1;
                out!("ERROR  {}: {}", name, e);
                continue;
            }
        };
        vectors += o.vectors();
        let parts: Vec<String> = o
            .got
            .iter()
            .enumerate()
            .map(|(k, (l, g))| match g {
                Ok(_) => format!("{} {}/{}", l, o.vectors() - o.differ(k).unwrap(), o.vectors()),
                Err(e) => format!("{}: {}", l, e),
            })
            .collect();
        let pass = (0..o.got.len()).all(|k| o.differ(k) == Some(0));
        let unused = if o.unused.is_empty() { String::new() } else { format!(" | not in the functions: {}", o.unused.join(" ")) };
        out!("{}  {}  {} inputs | {}{}{}", if pass { "OK  " } else { "FAIL" }, name, o.inputs.len(), parts.join(" | "), note, unused);
        if pass {
            ok += 1;
        } else {
            bad += 1;
            fails.push(name.clone());
        }
        if trace && !pass {
            if let Some(v) = (0..o.vectors()).find(|&v| o.got.iter().any(|(_, g)| g.as_ref().map_or(false, |g| g[v] != o.want[v]))) {
                for (l, c) in &circuits {
                    if let Ok(ph) = c.phases(&o.inputs, v) {
                        for (what, val) in &ph {
                            let nodes: Vec<String> = c.names.iter().zip(val).map(|(n, x)| format!("{}={}", n[0], level_char(*x))).collect();
                            out!("       trace {} vector {} {}: {}", l, v, what, nodes.join(" "));
                        }
                    }
                }
            }
        }
        if rows || !pass {
            let mut shown = 0;
            for v in 0..o.vectors() {
                let differs = o.got.iter().any(|(_, g)| g.as_ref().map_or(false, |g| g[v] != o.want[v]));
                if !rows && (!differs || shown >= 4) {
                    continue;
                }
                shown += 1;
                let ins: Vec<String> = o.inputs.iter().enumerate().map(|(k, n)| format!("{}={}", n, lsv::func::input_bit(v, k, o.inputs.len()))).collect();
                let lv = |r: &[u8]| o.outputs.iter().zip(r).map(|(n, x)| format!("{}={}", n, level_char(*x))).collect::<Vec<_>>().join(" ");
                let got: Vec<String> = o.got.iter().map(|(l, g)| format!("{} {}", l, g.as_ref().map(|g| lv(&g[v])).unwrap_or_else(|_| "-".into()))).collect();
                out!("       {}: want {} | {}{}", ins.join(" "), lv(&o.want[v]), got.join(" | "), if differs { "  <-" } else { "" });
            }
        }
    }
    out!(
        "summary: {} checked, {} OK, {} FAIL{}{}{} | {} sequential, {} without a function, {} not in the Liberty files | {} vectors | {:.1} s",
        ok + bad,
        ok,
        bad,
        if fails.is_empty() { String::new() } else { format!(" ({})", fails.iter().take(12).cloned().collect::<Vec<_>>().join(" ")) },
        if errors > 0 { format!(", {} ERROR (functions not checkable)", errors) } else { String::new() },
        if no_layout > 0 { format!(", {} without a layout check", no_layout) } else { String::new() },
        seq,
        nofunc,
        nolib,
        vectors,
        t0.elapsed().as_secs_f64()
    );
}
