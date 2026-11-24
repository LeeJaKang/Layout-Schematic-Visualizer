//! Transistor schematic of CDL cells as SVG, plus layout statistics over all cells.
//! usage: lsv-xtor <cdl> [--cell NAME --out file.svg]   (no --cell: statistics over every subckt)
use lsv::cdl::{flatten_subckt, parse_cdl};
use lsv::schematic::Colors;
use lsv::xtor::{crossings, layout, to_svg};
use eframe::egui::Color32;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let cdl = parse_cdl(&String::from_utf8_lossy(&std::fs::read(&a[0]).expect("cdl")), &a[0]);
    let get = |k: &str| a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).cloned();
    let col = Colors { ink: Color32::from_rgb(0x18, 0x22, 0x2e), wire: Color32::from_rgb(0x3b, 0x4a, 0x5c), muted: Color32::from_rgb(0x6b, 0x77, 0x85), hl: Color32::from_rgb(0xd4, 0x50, 0x0f), sel: Color32::from_rgb(0xd4, 0x50, 0x0f), fill: Color32::from_rgb(0xfd, 0xf0, 0xe8) };
    if let (Some(cell), Some(gds)) = (get("--cell"), get("--gds")) {
        // the whole cell tab (schematic + GDS + highlight) as SVG
        let files = lsv::cellview::CellFiles { cdl: Some(a[0].clone().into()), gds: Some(gds.into()), layermap: get("--layermap").map(|s| s.into()), lef: None, lpe: None, cell: Some(cell.clone()), libs: vec![] };
        let mut t = lsv::cellview::CellTab::new(files);
        t.load_sync().expect("load");
        t.show_cell(&cell);
        println!("{}", t.status);
        let sel = get("--sel").map(|s| match s.split_once(':') {
            Some(("dev", n)) => lsv::cellview::CSel::Dev(n.to_string()),
            Some(("net", n)) => lsv::cellview::CSel::Net(n.to_string()),
            _ => panic!("--sel dev:NAME or net:NAME"),
        });
        let svg = t.to_svg(1400.0, 700.0, sel, &col);
        std::fs::write(get("--out").expect("--out"), svg).expect("write");
        return;
    }
    if let Some(cell) = get("--cell") {
        let sc = flatten_subckt(&cdl, &cell).expect("subckt");
        let t = std::time::Instant::now();
        let lay = layout(&sc);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        println!("{}: {} devices, {} nodes, {} wires, size {:.0}x{:.0}, crossings {}, {:.2} ms", cell, sc.mos.len(), lay.nodes.len(), lay.wires.len(), lay.size.x, lay.size.y, crossings(&lay), ms);
        if let Some(out) = get("--out") {
            std::fs::write(&out, to_svg(&lay, &sc, None, &col)).expect("write");
        }
        return;
    }
    let t = std::time::Instant::now();
    let (mut n, mut cr, mut worst) = (0, 0, (0usize, String::new()));
    for name in &cdl.order {
        let sc = flatten_subckt(&cdl, name).expect("subckt");
        if sc.mos.is_empty() {
            continue;
        }
        let lay = layout(&sc);
        let c = crossings(&lay);
        n += 1;
        cr += c;
        if c > worst.0 {
            worst = (c, name.clone());
        }
    }
    println!("{} cells laid out, total crossings {}, worst {} ({}), {:.2} s", n, cr, worst.0, worst.1, t.elapsed().as_secs_f64());
}
