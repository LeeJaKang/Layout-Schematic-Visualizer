//! Draws a schematic to SVG with the viewer's layout and drawing code.
//! usage: lsv-sch --lib <lib>... --netlist <v> [--top T] [--cell NAME] [--fanin 2] [--fanout 1] [--select-net NAME] --out x.svg
//!        [--fold [--open INSTANCE_PATH]...]: the folded whole design (submodules as boxes) with the given ones opened
use eframe::egui::Color32;
use lsv::cells::analyze_cell;
use lsv::liberty::parse_liberty;
use lsv::netlist::{cone_cells, elaborate, pick_top};
use lsv::schematic::{layout, to_svg, Colors, Hit};
use lsv::verilog::parse_verilog;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (mut libs, mut net, mut top, mut cell, mut fi, mut fo, mut out, mut selnet) = (vec![], String::new(), None, None, 2usize, 1usize, String::from("sch.svg"), None);
    let mut skip_cross = false;
    let (mut fold, mut open) = (false, vec![]);
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--lib" => libs.push(v),
            "--netlist" => net = v,
            "--top" => top = Some(v),
            "--cell" => cell = Some(v),
            "--fanin" => fi = v.parse().unwrap(),
            "--fanout" => fo = v.parse().unwrap(),
            "--select-net" => selnet = Some(v),
            "--out" => out = v,
            "--no-svg" => {
                out = String::new();
                i -= 1;
            }
            "--no-crossings" => {
                skip_cross = true;
                i -= 1;
            }
            "--fold" => {
                fold = true;
                i -= 1;
            }
            "--open" => open.push(v),
            a => panic!("unknown argument {}", a),
        }
        i += 2;
    }
    let mut models = HashMap::new();
    for f in &libs {
        for c in parse_liberty(&lsv::io::read_all(std::path::Path::new(f)).unwrap(), f).unwrap() {
            models.insert(c.name.clone(), Arc::new(analyze_cell(&c)));
        }
    }
    let (order, mods) = parse_verilog(&lsv::io::read_all(std::path::Path::new(&net)).unwrap(), &net).unwrap();
    let top = top.unwrap_or_else(|| pick_top(&order, &mods)[0].clone());
    let nl = elaborate(&mods, &top, &models).unwrap();
    println!("liberty: {} cell models from {} files | netlist: {} modules, top {}, {} cell instances, {} nets | elaboration warnings: {}", models.len(), libs.len(), mods.len(), nl.top, nl.cells.len(), nl.net_by_name.len(), nl.warnings.len());
    for w in nl.warnings.iter().take(10) {
        println!("  warning: {}", w);
    }
    if fold {
        let t = Instant::now();
        let f = lsv::fold::Fold::build(&nl);
        let tb = t.elapsed().as_millis();
        let linked = f.linked.iter().filter(|&&x| x).count();
        println!("fold: {} groups ({} from instance names), {} cells placed by their connections, top sheet {} cells + {} boxes, build {} ms", f.groups.len() - 1, f.from_names, linked, f.groups[0].cells.len(), f.groups[0].children.len(), tb);
        let mut big: Vec<u32> = (1..f.groups.len() as u32).collect();
        big.sort_by_key(|&g| std::cmp::Reverse(f.groups[g as usize].total));
        for &g in big.iter().take(12) {
            let gr = &f.groups[g as usize];
            println!("  {:50} total {:7} direct {:7} boxes {:3} pins {:6}", gr.path, gr.total, gr.cells.len(), gr.children.len(), gr.pins.len());
        }
        if nl.cells.len() <= 200 {
            // small designs: every group with its cells (* = placed by its connections) and pins
            for gr in &f.groups {
                let cells: Vec<String> = gr.cells.iter().map(|&c| format!("{}{}", nl.cells[c as usize].name, if f.linked[c as usize] { "*" } else { "" })).collect();
                let pins: Vec<String> = gr.pins.iter().map(|p| format!("{}{}", if p.out { ">" } else { "<" }, p.name)).collect();
                println!("  [{}] {} cells: {} | pins: {}", if gr.path.is_empty() { "top" } else { &gr.path }, gr.module, cells.join(" "), pins.join(" "));
            }
        }
        let by_path: HashMap<&str, u32> = f.groups.iter().enumerate().map(|(i, g)| (g.path.as_str(), i as u32)).collect();
        let mut set = std::collections::HashSet::new();
        for p in &open {
            let g = *by_path.get(p.as_str()).unwrap_or_else(|| panic!("no group {}", p));
            set.extend(f.chain(g));
        }
        let mut cache = lsv::fold::Cache::new();
        let t = Instant::now();
        let lay = lsv::fold::view(&nl, &f, &set, &mut cache);
        let ms = t.elapsed().as_millis();
        let cells = lay.cell_node.len();
        println!("folded view: {} opened, {} cells and {} boxes or frames drawn, {} nodes, {} wires, size {:.0}x{:.0}, layout {} ms", set.len(), cells, lay.group_node.len(), lay.nodes.len(), lay.wires.len(), lay.size.x, lay.size.y, ms);
        // what is drawn: wire pieces, pin rows of the closed boxes, port and stub nodes, bundle wires
        use lsv::schematic::NodeKind;
        let pieces: usize = lay.wires.iter().map(|w| w.pts.len() - 1).sum();
        let (mut rows, mut pins, mut stubs) = (0usize, 0usize, 0usize);
        for n in &lay.nodes {
            match n.kind {
                NodeKind::Block(_) => {
                    for side in [&n.ins, &n.outs] {
                        let mut ys: Vec<u32> = side.iter().map(|x| x.1.to_bits()).collect();
                        ys.sort_unstable();
                        ys.dedup();
                        rows += ys.len();
                        pins += side.len();
                    }
                }
                NodeKind::InPort(_) | NodeKind::OutPort(_) | NodeKind::InStub(_) | NodeKind::OutStub(_) => stubs += 1,
                _ => {}
            }
        }
        let wb: Vec<&lsv::schematic::Bundle> = lay.bundles.iter().filter(|b| b.at.is_some()).collect();
        println!("drawn: {} wire pieces ({} of them bundle wires), box pins {} in {} rows, {} port and stub nodes, {} bundle wires carrying {} nets", pieces, lay.wires.iter().filter(|w| w.bundle).map(|w| w.pts.len() - 1).sum::<usize>(), pins, rows, stubs, wb.len(), wb.iter().map(|b| b.nets.len()).sum::<usize>());
        if !out.is_empty() {
            let col = Colors { ink: Color32::from_rgb(23, 33, 43), wire: Color32::from_rgb(52, 73, 94), muted: Color32::from_rgb(90, 103, 114), hl: Color32::from_rgb(212, 80, 15), sel: Color32::from_rgb(28, 79, 140), fill: Color32::from_rgb(248, 249, 246) };
            std::fs::write(&out, to_svg(&lay, &nl, None, &col)).unwrap();
        }
        return;
    }
    let cells: Vec<u32> = match &cell {
        Some(c) => cone_cells(&nl, &[nl.cell_index[c]], fi, fo),
        None => (0..nl.cells.len() as u32).collect(),
    };
    let t = Instant::now();
    let lay = layout(&nl, &cells);
    let ms = t.elapsed().as_millis();
    let sel = match (&selnet, &cell) {
        (Some(n), _) => nl.net_by_name.get(n).map(|&x| Hit::Net(x)),
        (None, Some(c)) => Some(Hit::Cell(nl.cell_index[c])),
        _ => None,
    };
    let col = Colors { ink: Color32::from_rgb(23, 33, 43), wire: Color32::from_rgb(52, 73, 94), muted: Color32::from_rgb(90, 103, 114), hl: Color32::from_rgb(212, 80, 15), sel: Color32::from_rgb(28, 79, 140), fill: Color32::from_rgb(248, 249, 246) };
    if !out.is_empty() {
        std::fs::write(&out, to_svg(&lay, &nl, sel, &col)).unwrap();
    }
    let crossings = if skip_cross { 0 } else {
        // count orthogonal crossings between different nets (vertical vs horizontal segments)
        let mut h = vec![];
        let mut v = vec![];
        for w in &lay.wires {
            for s in w.pts.windows(2) {
                if (s[0].y - s[1].y).abs() < 0.01 { h.push((w.net, s[0].x.min(s[1].x), s[0].x.max(s[1].x), s[0].y)); } else if (s[0].x - s[1].x).abs() < 0.01 { v.push((w.net, s[0].y.min(s[1].y), s[0].y.max(s[1].y), s[0].x)); }
            }
        }
        let mut n = 0;
        for a in &h {
            for b in &v {
                if a.0 != b.0 && b.3 > a.1 && b.3 < a.2 && a.3 > b.1 && a.3 < b.2 {
                    n += 1;
                }
            }
        }
        n
    };
    println!("{}: {} cells drawn of {}, {} nodes, {} wires, size {:.0}x{:.0}, wire crossings {}, layout {} ms -> {}", nl.top, cells.len(), nl.cells.len(), lay.nodes.len(), lay.wires.len(), lay.size.x, lay.size.y, crossings, ms, out);
}
