//! Prints the drawing model of every Liberty cell (kind and gate description per output), to compare with
//! the web viewer. usage: lsv-cells <lib>...
use lsv::cells::analyze_cell;
use lsv::liberty::parse_liberty;
use lsv::logic::describe_gate;

fn main() {
    let mut cells = vec![];
    for f in std::env::args().skip(1) {
        let text = lsv::io::read_all(std::path::Path::new(&f)).unwrap();
        cells.extend(parse_liberty(&text, &f).unwrap_or_else(|e| panic!("{}", e)));
    }
    let mut lines: Vec<String> = cells
        .iter()
        .map(|c| {
            let m = analyze_cell(c);
            let outs: Vec<String> = m.outputs.iter().map(|o| format!("{}={}", o, m.out_gates.get(o).map(describe_gate).unwrap_or_else(|| "-".into()))).collect();
            format!("{}\t{}\t{}\tclocks={}\t{}", m.name, m.kind_name(), m.kind_text, m.clocks.join(","), outs.join(" "))
        })
        .collect();
    lines.sort();
    for l in lines {
        println!("{}", l);
    }
}
