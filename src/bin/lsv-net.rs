//! Elaborates a netlist with the desktop readers and checks it against a DEF of the same design: every cell
//! pin must sit on the DEF net that lists (instance, pin).
//! usage: lsv-net --lib <lib>... --netlist <v[.gz]> [--top NAME] [--def <def> --lef <lef>...]
use lsv::cells::analyze_cell;
use lsv::def::parse_def;
use lsv::lef::{parse_lef, Lib};
use lsv::liberty::parse_liberty;
use lsv::netlist::{elaborate, pick_top};
use lsv::verilog::parse_verilog;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut libs, mut net, mut top, mut def, mut lefs) = (vec![], None, None, None, vec![]);
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--lib" => libs.push(PathBuf::from(v)),
            "--netlist" => net = Some(PathBuf::from(v)),
            "--top" => top = Some(v),
            "--def" => def = Some(PathBuf::from(v)),
            "--lef" => lefs.push(PathBuf::from(v)),
            a => panic!("unknown argument {}", a),
        }
        i += 2;
    }
    let t0 = Instant::now();
    let mut models = HashMap::new();
    for f in &libs {
        for c in parse_liberty(&lsv::io::read_all(f).unwrap(), &f.display().to_string()).unwrap() {
            models.insert(c.name.clone(), Arc::new(analyze_cell(&c)));
        }
    }
    let t1 = Instant::now();
    let net = net.expect("need --netlist");
    let src = lsv::io::read_all(&net).unwrap();
    let (order, mods) = parse_verilog(&src, &net.display().to_string()).unwrap_or_else(|e| panic!("{}", e));
    let t2 = Instant::now();
    let top = top.unwrap_or_else(|| pick_top(&order, &mods)[0].clone());
    let nl = elaborate(&mods, &top, &models).unwrap_or_else(|e| panic!("{}", e));
    let t3 = Instant::now();
    println!("libs {} cells {:.2} s | netlist parse {:.2} s ({} modules) | elaborate {:.2} s", models.len(), (t1 - t0).as_secs_f64(), (t2 - t1).as_secs_f64(), mods.len(), (t3 - t2).as_secs_f64());
    println!("{}: {} cells, {} nets, {} ports, {} submodule instances, warnings {}", nl.top, nl.cells.len(), nl.nets.len(), nl.ports.len(), nl.insts.len(), nl.warnings.len());
    for w in nl.warnings.iter().take(8) {
        println!("  warning: {}", w);
    }
    if std::env::var("LSV_DUMP").is_ok() {
        let mut lines = vec![];
        for c in &nl.cells {
            for p in &c.pins {
                lines.push(format!("{}\t{}\t{}\t{}", c.name, c.typ, p.pin, nl.nets[p.net as usize].name));
            }
        }
        for p in &nl.ports {
            lines.push(format!("PORT\t{}\t{}\t{}", p.name, p.dir, nl.nets[p.net as usize].name));
        }
        lines.sort();
        std::fs::write(std::env::var("LSV_DUMP").unwrap(), lines.join("\n") + "\n").unwrap();
    }
    if let Some(def) = def {
        let mut lib = Lib::default();
        for f in &lefs {
            parse_lef(&lsv::io::read_all(f).unwrap(), &f.display().to_string(), &mut lib).unwrap();
        }
        let text = lsv::io::read_all(&def).unwrap();
        let lay = parse_def(&text, &def.display().to_string(), &lib, std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4));
        drop(text);
        let mut def_pin: HashMap<(&str, &str), &str> = HashMap::new();
        for n in &lay.nets {
            for (c, p) in &n.conns {
                if c != "PIN" && c != "*" {
                    def_pin.insert((c.as_str(), p.as_str()), n.name.as_str());
                }
            }
        }
        let (mut ok, mut bad, mut missing, mut bus) = (0usize, 0usize, 0usize, 0usize);
        let mut ex = vec![];
        for c in &nl.cells {
            for p in &c.pins {
                if p.pin.ends_with(']') {
                    bus += 1;
                }
                match def_pin.get(&(c.name.as_str(), p.pin.as_str())) {
                    None => missing += 1,
                    Some(want) => {
                        let net = &nl.nets[p.net as usize];
                        if net.keys.iter().any(|&k| !nl.keys[k as usize].starts_with('#') && nl.key_display(k) == *want) {
                            ok += 1;
                        } else {
                            bad += 1;
                            if ex.len() < 5 {
                                ex.push(format!("{}.{}: netlist {} DEF {}", c.name, p.pin, net.name, want));
                            }
                        }
                    }
                }
            }
        }
        let found = nl.cells.iter().filter(|c| lay.comp_index.contains_key(&c.name)).count();
        println!("DEF {}: {} comps | netlist cells found in DEF {}/{}", lay.design, lay.comps.len(), found, nl.cells.len());
        println!("cell pins: {} on the same net as in DEF, {} different, {} not listed in DEF nets | bus bit pins {}", ok, bad, missing, bus);
        for x in ex {
            println!("  {}", x);
        }
    }
}
