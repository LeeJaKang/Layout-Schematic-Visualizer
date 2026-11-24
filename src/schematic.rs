//! Gate-level schematic: layered layout (Sugiyama style) of a set of cells, orthogonal wires in the gaps
//! between layers, gate symbols chosen from the Liberty function (same choice as the web viewer).
use crate::cells::Kind;
use crate::logic::{Src, GT};
use crate::netlist::{End, Netlist};
use eframe::egui::{self, Color32, Pos2, Rect, Shape, Stroke, Vec2};
use std::collections::{HashMap, HashSet};

pub const PITCH: f32 = 14.0;
const GAP_MIN: f32 = 60.0;
const TRACK: f32 = 7.0;
const VGAP: f32 = 22.0;

#[derive(Clone, Debug)]
pub enum NodeKind {
    Cell(u32),
    InPort(u32),
    OutPort(u32),
    InStub(u32),  // net entering the view from outside
    OutStub(u32), // net leaving the view
    Dummy,
    Block(u32), // closed submodule (group index in the fold)
    Frame(u32), // opened submodule: the frame around its sheet
}

#[derive(Clone, Debug)]
pub struct Node {
    pub kind: NodeKind,
    pub w: f32,
    pub h: f32,
    pub ins: Vec<(u32, f32)>,  // (net, y offset) of input pins
    pub outs: Vec<(u32, f32)>, // (net, y offset) of output pins
    pub pin_names_in: Vec<String>,
    pub pin_names_out: Vec<String>,
    pub layer: usize,
    pub pos: Pos2,
}

#[derive(Clone, Debug)]
pub struct Wire {
    pub net: u32,
    pub pts: Vec<Pos2>,
    /// drawn for a bundle of nets (`net` is the bundle's first net)
    pub bundle: bool,
}

/// nets drawn as one wire on a folded sheet (same pins at both ends), or a port or stub standing for several nets
#[derive(Clone, Debug)]
pub struct Bundle {
    pub rep: u32,
    pub nets: Vec<u32>,
    pub label: String,
    /// where the label goes (middle of the longest horizontal piece) and that piece's length
    pub at: Option<Pos2>,
    pub len: f32,
}

/// `name[12]` -> ("name", 12)
pub fn bus_base(s: &str) -> Option<(&str, i64)> {
    let t = s.strip_suffix(']')?;
    let k = t.rfind('[')?;
    let d = &t[k + 1..];
    if d.is_empty() || !d.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((&s[..k], d.parse().ok()?))
}

/// label for several nets or pins: `name[7:0]`, `name[0..9] (4 bits)` when the bits have gaps, or `5 nets`
pub fn bus_label(names: &[&str]) -> String {
    if names.len() == 1 {
        return names[0].to_string();
    }
    let parts: Vec<Option<(&str, i64)>> = names.iter().map(|n| bus_base(n)).collect();
    if let Some(Some((b0, _))) = parts.first() {
        if parts.iter().all(|p| matches!(p, Some((b, _)) if b == b0)) {
            let mut idx: Vec<i64> = parts.iter().map(|p| p.unwrap().1).collect();
            idx.sort_unstable();
            idx.dedup();
            let (lo, hi) = (idx[0], idx[idx.len() - 1]);
            return if (hi - lo + 1) as usize == idx.len() { format!("{}[{}:{}]", b0, hi, lo) } else { crate::trf!("{}[{}..{}] ({} bits)", b0, lo, hi, idx.len()) };
        }
    }
    crate::trf!("{} nets", names.len())
}

pub struct SchLayout {
    pub nodes: Vec<Node>,
    pub wires: Vec<Wire>,
    pub dots: Vec<(u32, Pos2)>,
    pub size: Vec2,
    pub cell_node: HashMap<u32, usize>,
    /// box or frame node of each submodule on the sheet
    pub group_node: HashMap<u32, usize>,
    pub labels: HashMap<u32, GroupText>,
    pub bundles: Vec<Bundle>,
}

/// what a box or frame shows about its submodule
#[derive(Clone, Debug)]
pub struct GroupText {
    pub title: String,
    pub path: String,
    pub cells: usize,
}

/// height of the title band of a box, and of a frame
pub const BOX_HEAD: f32 = 30.0;
pub const FRAME_HEAD: f32 = 30.0;

fn cell_pins(nl: &Netlist, ci: u32) -> (Vec<(u32, String)>, Vec<(u32, String)>) {
    let c = &nl.cells[ci as usize];
    let m = &c.model;
    let mut ins = vec![];
    let mut outs = vec![];
    // inputs in Liberty order, then any extra pins as connected
    let order: Vec<&String> = m.inputs.iter().chain(m.inouts.iter()).collect();
    for nm in order {
        if let Some(p) = c.pins.iter().find(|p| &p.pin == nm) {
            ins.push((p.net, p.pin.clone()));
        }
    }
    for p in &c.pins {
        if p.dir != "output" && !ins.iter().any(|x| x.1 == p.pin) {
            ins.push((p.net, p.pin.clone()));
        }
    }
    for nm in &m.outputs {
        if let Some(p) = c.pins.iter().find(|p| &p.pin == nm) {
            outs.push((p.net, p.pin.clone()));
        }
    }
    (ins, outs)
}

fn sym_size(nl: &Netlist, ci: u32, nin: usize, nout: usize) -> (f32, f32) {
    let m = &nl.cells[ci as usize].model;
    let rows = nin.max(nout).max(2) as f32;
    let h = (rows + 1.0) * PITCH;
    let w = match m.kind {
        Kind::Gate => 44.0,
        Kind::Tie => 30.0,
        Kind::Tri => 40.0,
        _ => (m.title.len() as f32 * 6.5 + 26.0).max(56.0),
    };
    (w, h)
}

/// the symbol node of one cell
pub fn cell_node(nl: &Netlist, ci: u32) -> Node {
    let (ins, outs) = cell_pins(nl, ci);
    let (w, h) = sym_size(nl, ci, ins.len(), outs.len());
    let iy = |i: usize, n: usize| if n == 1 { h / 2.0 } else { (i as f32 + 1.0) * h / (n as f32 + 1.0) };
    Node {
        kind: NodeKind::Cell(ci),
        w,
        h,
        ins: ins.iter().enumerate().map(|(i, x)| (x.0, iy(i, ins.len()))).collect(),
        outs: outs.iter().enumerate().map(|(i, x)| (x.0, iy(i, outs.len()))).collect(),
        pin_names_in: ins.iter().map(|x| x.1.clone()).collect(),
        pin_names_out: outs.iter().map(|x| x.1.clone()).collect(),
        layer: 0,
        pos: Pos2::ZERO,
    }
}

pub fn layout(nl: &Netlist, cells: &[u32]) -> SchLayout {
    let in_view: HashSet<u32> = cells.iter().copied().collect();
    let nodes: Vec<Node> = cells.iter().map(|&ci| cell_node(nl, ci)).collect();
    layout_nodes(nl, nodes, &|c| in_view.contains(&c), true, false, false)
}

/// layered layout of the given nodes (cells, closed submodule boxes, frames of opened ones). `inside(cell)`
/// says whether a cell is part of this sheet (directly or in a box on it). Nets that reach other cells get
/// stubs. On the top sheet (`root`) top ports are drawn as ports, otherwise as stubs. In a `frame` the
/// output stubs go to the last column, so the frame has its inputs on the left and its outputs on the right.
/// With `bundle` (folded sheets), ports of one bus share a node, stubs of one bus or of unnamed nets reaching
/// the same nodes share a node, and nets between the same pins of the same nodes are routed once as a bundle.
pub fn layout_nodes(nl: &Netlist, mut nodes: Vec<Node>, inside: &dyn Fn(u32) -> bool, root: bool, frame: bool, bundle: bool) -> SchLayout {
    let mut cell_node = HashMap::new();
    let mut group_node = HashMap::new();
    for (i, n) in nodes.iter().enumerate() {
        match n.kind {
            NodeKind::Cell(ci) => {
                cell_node.insert(ci, i);
            }
            NodeKind::Block(g) | NodeKind::Frame(g) => {
                group_node.insert(g, i);
            }
            _ => {}
        }
    }
    let n_items = nodes.len();
    // nets touching the view: drivers and sinks inside, ports and stubs for the rest
    let mut nets: Vec<u32> = vec![];
    let mut seen = HashSet::new();
    for n in &nodes {
        match n.kind {
            NodeKind::Cell(ci) => {
                for p in &nl.cells[ci as usize].pins {
                    if seen.insert(p.net) {
                        nets.push(p.net);
                    }
                }
            }
            _ => {
                for &(net, _) in n.ins.iter().chain(n.outs.iter()) {
                    if seen.insert(net) {
                        nets.push(net);
                    }
                }
            }
        }
    }
    let lbl_w = |s: &str| s.len() as f32 * 6.5 + 18.0;
    let mut driver_of: HashMap<u32, (usize, f32)> = HashMap::new(); // net -> (node, y offset)
    let mut sinks_of: HashMap<u32, Vec<(usize, f32)>> = HashMap::new();
    for (ni, n) in nodes.iter().enumerate() {
        for &(net, y) in &n.outs {
            driver_of.entry(net).or_insert((ni, y));
        }
        for &(net, y) in &n.ins {
            sinks_of.entry(net).or_default().push((ni, y));
        }
    }
    // folded sheets: the node a port or stub joins (bus name, or for unnamed nets the nodes reached, never cells)
    let is_cell: Vec<bool> = nodes.iter().map(|n| matches!(n.kind, NodeKind::Cell(_))).collect();
    let mut stub_group: HashMap<(bool, String), usize> = HashMap::new();
    let group_key = |net: u32, out: bool, port: bool, driver_of: &HashMap<u32, (usize, f32)>, sinks_of: &HashMap<u32, Vec<(usize, f32)>>| -> Option<(bool, String)> {
        let name = &nl.nets[net as usize].name;
        if let Some((b, _)) = bus_base(name) {
            return Some((out, format!("{}{}", if port { "P" } else { "B" }, b)));
        }
        if port {
            return None;
        }
        let mut ends: Vec<usize> = sinks_of.get(&net).map(|v| v.iter().map(|x| x.0).filter(|&v| v < is_cell.len()).collect()).unwrap_or_default();
        if out {
            ends.extend(driver_of.get(&net).map(|x| x.0));
        }
        ends.sort_unstable();
        ends.dedup();
        if ends.is_empty() || ends.iter().any(|&v| is_cell[v]) {
            return None;
        }
        Some((out, format!("S{:?}", ends)))
    };
    let mut merged: Vec<usize> = vec![];
    for &net in &nets {
        let n = &nl.nets[net as usize];
        let has_driver = driver_of.contains_key(&net);
        if !has_driver && sinks_of.contains_key(&net) {
            // driven by a port, a constant or a cell outside the view
            let port = if root { n.drivers.iter().find_map(|d| if let End::Port(p) = d { Some(*p) } else { None }) } else { None };
            let key = if bundle { group_key(net, false, port.is_some(), &driver_of, &sinks_of) } else { None };
            if let Some(&v) = key.as_ref().and_then(|k| stub_group.get(k)) {
                nodes[v].outs.push((net, PITCH * 0.75));
                driver_of.insert(net, (v, PITCH * 0.75));
                merged.push(v);
            } else {
                let name = n.name.clone();
                let kind = match port {
                    Some(p) => NodeKind::InPort(p),
                    None => NodeKind::InStub(net),
                };
                let w = lbl_w(&name);
                if let Some(k) = key {
                    stub_group.insert(k, nodes.len());
                }
                driver_of.insert(net, (nodes.len(), PITCH * 0.75));
                nodes.push(Node { kind, w, h: PITCH * 1.5, ins: vec![], outs: vec![(net, PITCH * 0.75)], pin_names_in: vec![], pin_names_out: vec![], layer: 0, pos: Pos2::ZERO });
            }
        }
        if has_driver {
            let outside = n.sinks.iter().any(|s| match s {
                End::Cell(c, _) => !inside(*c),
                End::Port(_) => true,
                _ => false,
            });
            if outside {
                let port = if root { n.sinks.iter().find_map(|s| if let End::Port(p) = s { Some(*p) } else { None }) } else { None };
                let key = if bundle { group_key(net, true, port.is_some(), &driver_of, &sinks_of) } else { None };
                if let Some(&v) = key.as_ref().and_then(|k| stub_group.get(k)) {
                    nodes[v].ins.push((net, PITCH * 0.75));
                    sinks_of.entry(net).or_default().push((v, PITCH * 0.75));
                    merged.push(v);
                } else {
                    let kind = match port {
                        Some(p) => NodeKind::OutPort(p),
                        None => NodeKind::OutStub(net),
                    };
                    let w = lbl_w(&n.name);
                    if let Some(k) = key {
                        stub_group.insert(k, nodes.len());
                    }
                    sinks_of.entry(net).or_default().push((nodes.len(), PITCH * 0.75));
                    nodes.push(Node { kind, w, h: PITCH * 1.5, ins: vec![(net, PITCH * 0.75)], outs: vec![], pin_names_in: vec![], pin_names_out: vec![], layer: 0, pos: Pos2::ZERO });
                }
            }
        }
    }
    // a port or stub standing for several nets is labelled with the bus range or the net count
    merged.sort_unstable();
    merged.dedup();
    for v in merged {
        let names: Vec<&str> = nodes[v].outs.iter().chain(nodes[v].ins.iter()).map(|x| nl.nets[x.0 as usize].name.as_str()).collect();
        let label = bus_label(&names);
        nodes[v].w = lbl_w(&label);
        if nodes[v].outs.is_empty() {
            nodes[v].pin_names_in = vec![label];
        } else {
            nodes[v].pin_names_out = vec![label];
        }
    }
    // edges node -> node (one per driver/sink pair)
    let nn = nodes.len();
    let mut succ: Vec<Vec<usize>> = vec![vec![]; nn];
    let mut has_edge: HashSet<(usize, usize)> = HashSet::new();
    for (net, sinks) in &sinks_of {
        if let Some(&(d, _)) = driver_of.get(net) {
            for &(s, _) in sinks {
                if s != d && has_edge.insert((d, s)) {
                    succ[d].push(s);
                }
            }
        }
    }
    // flip-flops and latches start a new level like inputs: edges into them do not count for layering,
    // otherwise every register on a path adds the depth of the logic behind it
    let seq: Vec<bool> = nodes.iter().map(|n| matches!(n.kind, NodeKind::Cell(ci) if nl.cells[ci as usize].model.kind == Kind::Seq)).collect();
    // break cycles: DFS, drop back edges for layering
    let mut state = vec![0u8; nn];
    let mut dag: Vec<Vec<usize>> = vec![vec![]; nn];
    for st in 0..nn {
        if state[st] != 0 {
            continue;
        }
        let mut stack = vec![(st, 0usize)];
        state[st] = 1;
        while let Some(&mut (v, ref mut i)) = stack.last_mut() {
            if *i < succ[v].len() {
                let w = succ[v][*i];
                *i += 1;
                if seq[w] {
                    continue;
                }
                if state[w] == 0 {
                    dag[v].push(w);
                    state[w] = 1;
                    stack.push((w, 0));
                } else if state[w] == 2 {
                    dag[v].push(w);
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    // longest-path layering, input ports and stubs in layer 0, output ports after their driver. On large
    // sheets a layer holds at most `cap` nodes (filled in topological order, the rest moves on), so the
    // sheet stays about as wide as it is high instead of a few very tall columns.
    let mut indeg = vec![0usize; nn];
    for v in 0..nn {
        for &w in &dag[v] {
            indeg[w] += 1;
        }
    }
    let mut q: Vec<usize> = (0..nn).filter(|&v| indeg[v] == 0).collect();
    let mut qi = 0;
    while qi < q.len() {
        let v = q[qi];
        qi += 1;
        for &w in &dag[v] {
            indeg[w] -= 1;
            if indeg[w] == 0 {
                q.push(w);
            }
        }
    }
    let cap = if n_items > 2000 || (bundle && n_items > 300) { ((nn as f32 / 0.4).sqrt() as usize).max(60) } else { usize::MAX };
    let mut layer = vec![0usize; nn];
    let mut minl = vec![0usize; nn];
    let mut count: Vec<usize> = vec![];
    for &v in &q {
        let mut l = minl[v];
        if matches!(nodes[v].kind, NodeKind::InPort(_) | NodeKind::InStub(_)) {
            l = 0;
        } else {
            while count.get(l).copied().unwrap_or(0) >= cap {
                l += 1;
            }
        }
        if count.len() <= l {
            count.resize(l + 1, 0);
        }
        count[l] += 1;
        layer[v] = l;
        for &w in &dag[v] {
            minl[w] = minl[w].max(l + 1);
        }
    }
    // output ports and stubs sit right after their driver
    let mut pred_layer = vec![0usize; nn];
    for v in 0..nn {
        for &w in &succ[v] {
            pred_layer[w] = pred_layer[w].max(layer[v] + 1);
        }
    }
    for (v, n) in nodes.iter().enumerate() {
        if matches!(n.kind, NodeKind::OutPort(_) | NodeKind::OutStub(_)) {
            layer[v] = pred_layer[v].max(1);
        }
        if matches!(n.kind, NodeKind::InPort(_) | NodeKind::InStub(_)) {
            layer[v] = 0;
        }
    }
    if frame {
        // inside a frame every output stub is in one last column at the right edge
        let last = nodes.iter().enumerate().filter(|(_, n)| !matches!(n.kind, NodeKind::OutStub(_))).map(|(v, _)| layer[v]).max().unwrap_or(0) + 1;
        for (v, n) in nodes.iter().enumerate() {
            if matches!(n.kind, NodeKind::OutStub(_)) {
                layer[v] = last;
            }
        }
    }
    let nl_layers = layer.iter().copied().max().unwrap_or(0) + 1;
    // one dummy per net and layer between its driver and its furthest forward sink: all long edges of a
    // net share them, so a net crosses a layer once however many sinks it has
    struct NetRoute {
        net: u32,
        drv: usize,
        dy: f32,
        maxl: usize,
        dummies: HashMap<usize, usize>, // layer -> dummy node
        fwd: Vec<(usize, f32)>,         // sinks in later layers
        back: Vec<(usize, f32)>,        // sinks in the same or an earlier layer (feedback)
    }
    // folded sheets: nets running between the same pins of the same nodes are routed once, as a bundle
    let mut rep_of: HashMap<u32, u32> = HashMap::new();
    let mut members: HashMap<u32, Vec<u32>> = HashMap::new();
    if bundle {
        let mut by_key: HashMap<Vec<(usize, u32)>, u32> = HashMap::new();
        for &net in &nets {
            if let (Some(&(d, dy)), Some(sinks)) = (driver_of.get(&net), sinks_of.get(&net)) {
                let mut key: Vec<(usize, u32)> = sinks.iter().map(|&(v, y)| (v, y.to_bits())).collect();
                key.sort_unstable();
                key.dedup();
                key.insert(0, (d, dy.to_bits()));
                let r = *by_key.entry(key).or_insert(net);
                rep_of.insert(net, r);
                members.entry(r).or_default().push(net);
            }
        }
    }
    let bundled: HashSet<u32> = members.iter().filter(|(_, m)| m.len() > 1).map(|(&r, _)| r).collect();
    let mut routes: Vec<NetRoute> = vec![];
    for &net in &nets {
        if rep_of.get(&net).map_or(false, |&r| r != net) {
            continue;
        }
        let (d, dy) = match driver_of.get(&net) {
            Some(x) => *x,
            None => continue,
        };
        let sinks = match sinks_of.get(&net) {
            Some(s) => s,
            None => continue,
        };
        let ld = layer[d];
        let (mut fwd, mut back) = (vec![], vec![]);
        for &(s, sy) in sinks {
            if s == d {
                continue;
            }
            if layer[s] > ld {
                fwd.push((s, sy));
            } else {
                back.push((s, sy));
            }
        }
        if fwd.is_empty() && back.is_empty() {
            continue;
        }
        let maxl = fwd.iter().map(|&(s, _)| layer[s]).max().unwrap_or(ld);
        let mut dummies = HashMap::new();
        for l in ld + 1..maxl {
            let id = nodes.len();
            nodes.push(Node { kind: NodeKind::Dummy, w: 0.0, h: 0.0, ins: vec![(net, 0.0)], outs: vec![(net, 0.0)], pin_names_in: vec![], pin_names_out: vec![], layer: l, pos: Pos2::ZERO });
            layer.push(l);
            dummies.insert(l, id);
        }
        routes.push(NetRoute { net, drv: d, dy, maxl, dummies, fwd, back });
    }
    let nn = nodes.len();
    for (v, n) in nodes.iter_mut().enumerate() {
        n.layer = layer[v];
    }
    // edges between adjacent layers
    let mut adj_up: Vec<Vec<(usize, u32)>> = vec![vec![]; nn]; // neighbours in the previous layer
    let mut adj_dn: Vec<Vec<(usize, u32)>> = vec![vec![]; nn]; // neighbours in the next layer
    for r in &routes {
        let ld = layer[r.drv];
        let src_at = |l: usize| -> usize { if l == ld { r.drv } else { r.dummies[&l] } };
        for l in ld + 1..r.maxl {
            let (a, b) = (src_at(l - 1), r.dummies[&l]);
            adj_dn[a].push((b, r.net));
            adj_up[b].push((a, r.net));
        }
        for &(s, _) in &r.fwd {
            let a = src_at(layer[s] - 1);
            adj_dn[a].push((s, r.net));
            adj_up[s].push((a, r.net));
        }
    }
    // order inside layers: barycenter sweeps
    let mut layers: Vec<Vec<usize>> = vec![vec![]; nl_layers];
    for v in 0..nn {
        layers[layer[v]].push(v);
    }
    let mut pos_in_layer = vec![0f32; nn];
    for l in &layers {
        for (i, &v) in l.iter().enumerate() {
            pos_in_layer[v] = i as f32;
        }
    }
    for it in 0..8 {
        let downward = it % 2 == 0;
        let range: Vec<usize> = if downward { (1..nl_layers).collect() } else { (0..nl_layers.saturating_sub(1)).rev().collect() };
        for l in range {
            let mut keyed: Vec<(f32, usize)> = layers[l]
                .iter()
                .map(|&v| {
                    let nb = if downward { &adj_up[v] } else { &adj_dn[v] };
                    let k = if nb.is_empty() { pos_in_layer[v] } else { nb.iter().map(|&(u, _)| pos_in_layer[u]).sum::<f32>() / nb.len() as f32 };
                    (k, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            layers[l] = keyed.into_iter().map(|x| x.1).collect();
            for (i, &v) in layers[l].iter().enumerate() {
                pos_in_layer[v] = i as f32;
            }
        }
    }
    // y: each node moves towards the pins it connects to in the neighbouring layer, keeping the order and
    // the spacing. Solved per layer as a weighted least-squares problem under the order constraints
    // (pool adjacent violators, exact and linear in the layer size). Dummies weigh most, so long
    // wires stay straight.
    // pins of nodes with many of them (submodule boxes) by net, the first pin of a net as in the scan below
    let mut many: HashMap<(usize, bool, u32), f32> = HashMap::new();
    for (v, n) in nodes.iter().enumerate() {
        if n.ins.len() + n.outs.len() > 16 {
            for &(net, y) in &n.ins {
                many.entry((v, false, net)).or_insert(y);
            }
            for &(net, y) in &n.outs {
                many.entry((v, true, net)).or_insert(y);
            }
        }
    }
    let pin_off = |nodes: &Vec<Node>, v: usize, net: u32, out: bool| -> f32 {
        let n = &nodes[v];
        if n.ins.len() + n.outs.len() > 16 {
            return many.get(&(v, out, net)).copied().unwrap_or(n.h / 2.0);
        }
        let list = if out { &n.outs } else { &n.ins };
        list.iter().find(|x| x.0 == net).map(|x| x.1).unwrap_or(n.h / 2.0)
    };
    let is_dummy = |nodes: &Vec<Node>, v: usize| matches!(nodes[v].kind, NodeKind::Dummy);
    let sep = |nodes: &Vec<Node>, a: usize, b: usize| if is_dummy(nodes, a) && is_dummy(nodes, b) { TRACK + 1.0 } else if is_dummy(nodes, a) || is_dummy(nodes, b) { VGAP * 0.6 } else { VGAP };
    for l in 0..nl_layers {
        let mut y = 40.0;
        for (i, &v) in layers[l].iter().enumerate() {
            if i > 0 {
                y += sep(&nodes, layers[l][i - 1], v);
            }
            nodes[v].pos.y = y;
            y += nodes[v].h;
        }
    }
    // the sweeps read positions, heights and pin offsets from flat arrays (same arithmetic as on the nodes)
    let mut py: Vec<f32> = nodes.iter().map(|n| n.pos.y).collect();
    let hh: Vec<f32> = nodes.iter().map(|n| n.h).collect();
    let up_off: Vec<Vec<(usize, f32, f32)>> = (0..nn).map(|v| adj_up[v].iter().map(|&(u, net)| (u, pin_off(&nodes, u, net, true), pin_off(&nodes, v, net, false))).collect()).collect();
    let dn_off: Vec<Vec<(usize, f32, f32)>> = (0..nn).map(|v| adj_dn[v].iter().map(|&(u, net)| (u, pin_off(&nodes, u, net, false), pin_off(&nodes, v, net, true))).collect()).collect();
    for sweep in 0..8 {
        let downward = sweep % 2 == 0;
        let ls: Vec<usize> = if downward { (1..nl_layers).collect() } else { (0..nl_layers.saturating_sub(1)).rev().collect() };
        for l in ls {
            let lv = &layers[l];
            let n = lv.len();
            if n == 0 {
                continue;
            }
            // blocks of (weight, weight * value, count) for z_i = top_i - c_i, which must not decrease
            let mut blocks: Vec<(f64, f64, usize)> = Vec::with_capacity(n);
            let mut offs: Vec<f64> = Vec::with_capacity(n);
            let mut c = 0.0f64;
            for i in 0..n {
                let v = lv[i];
                let nb = if downward { &up_off[v] } else { &dn_off[v] };
                let (want, wgt) = if nb.is_empty() {
                    (py[v] as f64, 0.05)
                } else {
                    let mut sum = 0.0f64;
                    for &(u, du, dv) in nb {
                        sum += (py[u] + du - dv) as f64;
                    }
                    (sum / nb.len() as f64, if is_dummy(&nodes, v) { 50.0 } else { nb.len() as f64 })
                };
                offs.push(c);
                blocks.push((wgt, wgt * (want - c), 1));
                while blocks.len() >= 2 {
                    let k = blocks.len();
                    let (a, b) = (blocks[k - 2], blocks[k - 1]);
                    if a.1 / a.0 > b.1 / b.0 {
                        blocks.truncate(k - 2);
                        blocks.push((a.0 + b.0, a.1 + b.1, a.2 + b.2));
                    } else {
                        break;
                    }
                }
                c += hh[v] as f64 + if i + 1 < n { sep(&nodes, v, lv[i + 1]) as f64 } else { 0.0 };
            }
            let mut i = 0;
            for (w, wy, cnt) in blocks {
                let z = wy / w;
                for _ in 0..cnt {
                    py[lv[i]] = (z + offs[i]) as f32;
                    i += 1;
                }
            }
        }
    }
    for (n, &y) in nodes.iter_mut().zip(py.iter()) {
        n.pos.y = y;
    }
    // shift everything to positive y
    let miny = nodes.iter().map(|n| n.pos.y).fold(f32::INFINITY, f32::min);
    for n in nodes.iter_mut() {
        n.pos.y += 40.0 - miny;
    }
    let bottom = nodes.iter().map(|n| n.pos.y + n.h).fold(0.0, f32::max) + 24.0;
    // vertical tracks in each gap: one trunk per net, nets whose spans do not overlap share a track
    let y_of = |nodes: &Vec<Node>, v: usize, off: f32| nodes[v].pos.y + off;
    let mut fb_lane: HashMap<usize, f32> = HashMap::new(); // route -> y of its lane below the drawing
    let mut k = 0;
    for (ri, r) in routes.iter().enumerate() {
        if !r.back.is_empty() {
            fb_lane.insert(ri, bottom + k as f32 * TRACK);
            k += 1;
        }
    }
    let mut spans: Vec<Vec<(f32, f32, usize)>> = vec![vec![]; nl_layers]; // gap -> (ymin, ymax, route)
    for (ri, r) in routes.iter().enumerate() {
        let ld = layer[r.drv];
        for l in ld..r.maxl {
            let sy = if l == ld { y_of(&nodes, r.drv, r.dy) } else { nodes[r.dummies[&l]].pos.y };
            let (mut a, mut b) = (sy, sy);
            if let Some(&d) = r.dummies.get(&(l + 1)) {
                a = a.min(nodes[d].pos.y);
                b = b.max(nodes[d].pos.y);
            }
            for &(s, off) in &r.fwd {
                if layer[s] == l + 1 {
                    let t = y_of(&nodes, s, off);
                    a = a.min(t);
                    b = b.max(t);
                }
            }
            spans[l].push((a, b, ri));
        }
        if let Some(&lane) = fb_lane.get(&ri) {
            spans[ld].push((y_of(&nodes, r.drv, r.dy), lane, ri));
        }
    }
    let mut track: HashMap<(usize, usize), usize> = HashMap::new(); // (gap, route) -> track
    let mut ntracks = vec![0usize; nl_layers];
    for l in 0..nl_layers {
        let sp = &mut spans[l];
        sp.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.2.cmp(&b.2)));
        let mut free: std::collections::BinaryHeap<std::cmp::Reverse<(i64, usize)>> = std::collections::BinaryHeap::new();
        let mut used = 0usize;
        for &(a, b, ri) in sp.iter() {
            let t = match free.peek() {
                Some(&std::cmp::Reverse((end, t))) if (end as f32) + TRACK < a => {
                    free.pop();
                    t
                }
                _ => {
                    used += 1;
                    used - 1
                }
            };
            track.insert((l, ri), t);
            free.push(std::cmp::Reverse((b.ceil() as i64, t)));
        }
        ntracks[l] = used;
    }
    // feedback entries: one vertical per net and sink layer, just left of that layer
    let mut entry_idx: HashMap<(usize, usize), usize> = HashMap::new(); // (route, layer) -> index
    let mut nentries = vec![0usize; nl_layers];
    for (ri, r) in routes.iter().enumerate() {
        let mut ls: Vec<usize> = r.back.iter().map(|&(s, _)| layer[s]).collect();
        ls.sort_unstable();
        ls.dedup();
        for l in ls {
            entry_idx.insert((ri, l), nentries[l]);
            nentries[l] += 1;
        }
    }
    let entry_w = |l: usize| if nentries[l] > 0 { nentries[l] as f32 * TRACK + 10.0 } else { 0.0 };
    // x of the layers from their widths, the tracks of each gap and the feedback entries
    let layer_w: Vec<f32> = layers.iter().map(|l| l.iter().map(|&v| nodes[v].w).fold(0.0, f32::max)).collect();
    let gap_w: Vec<f32> = ntracks.iter().map(|&t| ((t as f32 + 1.0) * TRACK + 30.0).max(GAP_MIN)).collect();
    let mut layer_x = vec![0f32; nl_layers];
    let mut x = 40.0 + entry_w(0);
    for l in 0..nl_layers {
        layer_x[l] = x;
        x += layer_w[l] + gap_w[l] + if l + 1 < nl_layers { entry_w(l + 1) } else { 0.0 };
    }
    for l in 0..nl_layers {
        for &v in &layers[l] {
            nodes[v].pos.x = layer_x[l] + if matches!(nodes[v].kind, NodeKind::InPort(_) | NodeKind::InStub(_)) { layer_w[l] - nodes[v].w } else { 0.0 };
        }
    }
    // wires: per net and gap a source stub, the trunk, and a stub to every target; dummies pass straight
    // through their layer; feedback sinks go down to the net's lane below the drawing and back up
    let mut wires = vec![];
    let seg = |wires: &mut Vec<Wire>, net: u32, a: Pos2, b: Pos2| {
        if a != b {
            wires.push(Wire { net, pts: vec![a, b], bundle: bundled.contains(&net) });
        }
    };
    for (ri, r) in routes.iter().enumerate() {
        let ld = layer[r.drv];
        let drv_pin = nodes[r.drv].pos + Vec2::new(nodes[r.drv].w, r.dy);
        for l in ld..r.maxl {
            let src = if l == ld { drv_pin } else { let d = r.dummies[&l]; Pos2::new(layer_x[l] + layer_w[l], nodes[d].pos.y) };
            let tx = layer_x[l] + layer_w[l] + 15.0 + TRACK * track[&(l, ri)] as f32;
            let mut targets: Vec<Pos2> = vec![];
            if let Some(&d) = r.dummies.get(&(l + 1)) {
                targets.push(Pos2::new(layer_x[l + 1], nodes[d].pos.y));
            }
            for &(s, off) in &r.fwd {
                if layer[s] == l + 1 {
                    targets.push(nodes[s].pos + Vec2::new(0.0, off));
                }
            }
            let (mut a, mut b) = (src.y, src.y);
            for t in &targets {
                a = a.min(t.y);
                b = b.max(t.y);
            }
            seg(&mut wires, r.net, src, Pos2::new(tx, src.y));
            seg(&mut wires, r.net, Pos2::new(tx, a), Pos2::new(tx, b));
            for t in targets {
                seg(&mut wires, r.net, Pos2::new(tx, t.y), t);
            }
        }
        for (&l, &d) in &r.dummies {
            let y = nodes[d].pos.y;
            seg(&mut wires, r.net, Pos2::new(layer_x[l], y), Pos2::new(layer_x[l] + layer_w[l], y));
        }
        if let Some(&lane) = fb_lane.get(&ri) {
            let tx = layer_x[ld] + layer_w[ld] + 15.0 + TRACK * track[&(ld, ri)] as f32;
            seg(&mut wires, r.net, drv_pin, Pos2::new(tx, drv_pin.y));
            seg(&mut wires, r.net, Pos2::new(tx, drv_pin.y), Pos2::new(tx, lane));
            let mut ls: Vec<usize> = r.back.iter().map(|&(s, _)| layer[s]).collect();
            ls.sort_unstable();
            ls.dedup();
            for l in ls {
                let ex = layer_x[l] - 10.0 - TRACK * entry_idx[&(ri, l)] as f32;
                let pts: Vec<Pos2> = r.back.iter().filter(|&&(s, _)| layer[s] == l).map(|&(s, off)| nodes[s].pos + Vec2::new(0.0, off)).collect();
                let top = pts.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
                seg(&mut wires, r.net, Pos2::new(tx, lane), Pos2::new(ex, lane));
                seg(&mut wires, r.net, Pos2::new(ex, lane), Pos2::new(ex, top));
                for p in pts {
                    seg(&mut wires, r.net, Pos2::new(ex, p.y), p);
                }
            }
        }
    }
    let dots = junctions(&wires);
    let w = nodes.iter().map(|n| n.pos.x + n.w).fold(0.0, f32::max) + 40.0;
    let h = wires.iter().flat_map(|w| w.pts.iter().map(|p| p.y)).chain(nodes.iter().map(|n| n.pos.y + n.h)).fold(0.0, f32::max) + 40.0;
    let mut bundles = vec![];
    if bundle {
        // label of each bundle wire on its longest horizontal piece
        let mut best: HashMap<u32, (Pos2, f32)> = HashMap::new();
        for wr in wires.iter().filter(|wr| wr.bundle) {
            for p in wr.pts.windows(2) {
                let len = (p[1].x - p[0].x).abs();
                if (p[1].y - p[0].y).abs() < 0.01 && best.get(&wr.net).map_or(true, |b| len > b.1) {
                    best.insert(wr.net, (Pos2::new((p[0].x + p[1].x) / 2.0, p[0].y), len));
                }
            }
        }
        let mut reps: Vec<u32> = bundled.iter().copied().collect();
        reps.sort_unstable();
        for r in reps {
            let m = &members[&r];
            let names: Vec<&str> = m.iter().map(|&x| nl.nets[x as usize].name.as_str()).collect();
            let (at, len) = best.get(&r).map(|b| (Some(b.0), b.1)).unwrap_or((None, 0.0));
            bundles.push(Bundle { rep: r, nets: m.clone(), label: bus_label(&names), at, len });
        }
        for n in &nodes {
            if matches!(n.kind, NodeKind::InPort(_) | NodeKind::OutPort(_) | NodeKind::InStub(_) | NodeKind::OutStub(_)) && n.ins.len() + n.outs.len() > 1 {
                let nets: Vec<u32> = n.outs.iter().chain(n.ins.iter()).map(|x| x.0).collect();
                let label = n.pin_names_out.first().or(n.pin_names_in.first()).cloned().unwrap_or_default();
                bundles.push(Bundle { rep: nets[0], nets, label, at: None, len: 0.0 });
            }
        }
    }
    SchLayout { nodes, wires, dots, size: Vec2::new(w, h), cell_node, group_node, labels: HashMap::new(), bundles }
}

/// label and member nets of the bundle(s) with this first net (a bundle wire and the port or stub it starts at)
pub fn bundle_nets(lay: &SchLayout, rep: u32) -> (String, Vec<u32>) {
    let mut label = String::new();
    let mut nets: Vec<u32> = vec![];
    let mut seen = HashSet::new();
    for b in lay.bundles.iter().filter(|b| b.rep == rep) {
        if label.is_empty() || b.at.is_some() {
            label = b.label.clone();
        }
        nets.extend(b.nets.iter().copied().filter(|&n| seen.insert(n)));
    }
    (label, nets)
}

/// a point on the drawing of a net: its own wire, or the bundle wire that carries it
pub fn net_point(lay: &SchLayout, net: u32) -> Option<Pos2> {
    let w = lay.wires.iter().find(|w| w.net == net && !w.bundle).or_else(|| {
        let r = lay.bundles.iter().find(|b| b.nets.contains(&net))?.rep;
        lay.wires.iter().find(|w| w.bundle && w.net == r)
    })?;
    Some(w.pts[w.pts.len() / 2])
}

/// copies a laid-out sheet into another one, moved by `off` (dummy nodes are left out)
pub fn append(dst: &mut SchLayout, src: &SchLayout, off: Vec2) {
    let mut idx = vec![usize::MAX; src.nodes.len()];
    for (i, n) in src.nodes.iter().enumerate() {
        if matches!(n.kind, NodeKind::Dummy) {
            continue;
        }
        idx[i] = dst.nodes.len();
        let mut m = n.clone();
        m.pos += off;
        dst.nodes.push(m);
    }
    for w in &src.wires {
        dst.wires.push(Wire { net: w.net, pts: w.pts.iter().map(|&p| p + off).collect(), bundle: w.bundle });
    }
    dst.dots.extend(src.dots.iter().map(|&(n, p)| (n, p + off)));
    for (&c, &i) in &src.cell_node {
        dst.cell_node.insert(c, idx[i]);
    }
    for (&g, &i) in &src.group_node {
        dst.group_node.insert(g, idx[i]);
    }
    for (&g, t) in &src.labels {
        dst.labels.insert(g, t.clone());
    }
    for b in &src.bundles {
        dst.bundles.push(Bundle { at: b.at.map(|p| p + off), ..b.clone() });
    }
}

/// screen rectangle of the + button of a box, or the - button of a frame, when the node is large enough on
/// screen to carry one
pub fn fold_button(n: &Node, view: &View, rect: Rect) -> Option<Rect> {
    let z = view.zoom;
    let r = Rect::from_min_size(view.to_screen(rect, n.pos), Vec2::new(n.w, n.h) * z);
    if r.width() < 40.0 || r.height() < 24.0 {
        return None;
    }
    let s = (13.0 * z).clamp(12.0, 18.0);
    match n.kind {
        NodeKind::Block(_) => Some(Rect::from_min_size(Pos2::new(r.max.x - 8.0 * z - s - 3.0, r.min.y + 3.0), Vec2::splat(s))),
        NodeKind::Frame(_) => Some(Rect::from_min_size(Pos2::new(r.min.x + 6.0, r.min.y + 6.0), Vec2::splat(s))),
        _ => None,
    }
}

/// the submodule whose + or - button is under the screen point
pub fn button_at(lay: &SchLayout, view: &View, rect: Rect, p: Pos2) -> Option<u32> {
    let mut hit = None;
    for &i in lay.group_node.values() {
        let n = &lay.nodes[i];
        if let (NodeKind::Block(g) | NodeKind::Frame(g), Some(b)) = (n.kind.clone(), fold_button(n, view, rect)) {
            if b.expand(2.0).contains(p) {
                // nested frames share a corner area: the innermost (smallest) wins
                if hit.map(|(_, w): (u32, f32)| n.w < w).unwrap_or(true) {
                    hit = Some((g, n.w));
                }
            }
        }
    }
    hit.map(|h| h.0)
}

/// junction dots: per net, the unique wire segments split at every vertex of that net, and the points
/// where three or more of those pieces meet
fn junctions(wires: &[Wire]) -> Vec<(u32, Pos2)> {
    let key = |p: Pos2| ((p.x * 2.0).round() as i64, (p.y * 2.0).round() as i64);
    let mut by_net: HashMap<u32, Vec<((i64, i64), (i64, i64))>> = HashMap::new();
    for w in wires {
        let segs = by_net.entry(w.net).or_default();
        for s in w.pts.windows(2) {
            let (a, b) = (key(s[0]), key(s[1]));
            if a != b {
                segs.push(if a < b { (a, b) } else { (b, a) });
            }
        }
    }
    let mut out = vec![];
    for (net, mut segs) in by_net {
        segs.sort();
        segs.dedup();
        let mut verts: Vec<(i64, i64)> = segs.iter().flat_map(|s| [s.0, s.1]).collect();
        verts.sort();
        verts.dedup();
        // the same vertices ordered by (y, x), for horizontal segments
        let mut verts_yx: Vec<(i64, i64)> = verts.iter().map(|v| (v.1, v.0)).collect();
        verts_yx.sort();
        let mut deg: HashMap<(i64, i64), usize> = HashMap::new();
        let mut pieces = HashSet::new();
        for &(a, b) in &segs {
            // vertices lying strictly inside this axis-aligned segment split it
            let mut cuts: Vec<(i64, i64)> = if a.0 == b.0 {
                let (lo, hi) = (a.1.min(b.1), a.1.max(b.1));
                let s0 = verts.partition_point(|&v| v <= (a.0, lo));
                let s1 = verts.partition_point(|&v| v < (a.0, hi));
                verts[s0..s1.max(s0)].to_vec()
            } else if a.1 == b.1 {
                let (lo, hi) = (a.0.min(b.0), a.0.max(b.0));
                let s0 = verts_yx.partition_point(|&v| v <= (a.1, lo));
                let s1 = verts_yx.partition_point(|&v| v < (a.1, hi));
                verts_yx[s0..s1.max(s0)].iter().map(|v| (v.1, v.0)).collect()
            } else {
                vec![]
            };
            cuts.push(a);
            cuts.push(b);
            cuts.sort();
            for c in cuts.windows(2) {
                pieces.insert((c[0], c[1]));
            }
        }
        for (a, b) in pieces {
            *deg.entry(a).or_insert(0) += 1;
            *deg.entry(b).or_insert(0) += 1;
        }
        for (v, d) in deg {
            if d >= 3 {
                out.push((net, Pos2::new(v.0 as f32 / 2.0, v.1 as f32 / 2.0)));
            }
        }
    }
    out
}

// ---------- GPU overview ----------
// Far out, the whole sheet is drawn as rectangles by the layout renderer: every wire piece is
// horizontal or vertical, so it is a thin rectangle, and cells are boxes. Integer coordinates, y up.

pub const GPU_SCALE: f32 = 8.0;
pub const SLOT_WIRE: u32 = 0;
pub const SLOT_CELL: u32 = 1;
pub const SLOT_PORT: u32 = 2;
pub const SLOT_SEQ: u32 = 3;
pub const SLOT_HL_LINE: u32 = 4;
pub const SLOT_HL_BOX: u32 = 5;
/// number of slots drawn from the uploaded instances (the highlight slots come with each frame)
pub const GPU_SLOTS: usize = 4;

fn gpu_rect(a: Pos2, b: Pos2) -> [i32; 4] {
    let s = GPU_SCALE;
    [(a.x.min(b.x) * s).round() as i32, (-a.y.max(b.y) * s).round() as i32, (a.x.max(b.x) * s).round() as i32, (-a.y.min(b.y) * s).round() as i32]
}

/// instances sorted by slot (wires, logic cells, ports, flip-flops and latches) and the range of each slot
pub fn gpu_insts(lay: &SchLayout, nl: &Netlist) -> (Vec<crate::scene::Inst>, Vec<(u32, u32)>) {
    use crate::scene::Inst;
    let mut v: Vec<Inst> = Vec::with_capacity(lay.wires.len() * 2 + lay.nodes.len());
    let mut ranges = vec![(0u32, 0u32); GPU_SLOTS];
    for w in lay.wires.iter().filter(|w| !w.bundle) {
        for s in w.pts.windows(2) {
            v.push(Inst { r: gpu_rect(s[0], s[1]), slot: SLOT_WIRE });
        }
    }
    ranges[SLOT_WIRE as usize] = (0, v.len() as u32);
    let is_seq = |ci: u32| nl.cells[ci as usize].model.kind == Kind::Seq;
    let body = |n: &Node| gpu_rect(n.pos + Vec2::new(8.0, 0.0), n.pos + Vec2::new(n.w - 8.0, n.h));
    let a = v.len() as u32;
    for n in &lay.nodes {
        match n.kind {
            NodeKind::Cell(ci) if !is_seq(ci) => v.push(Inst { r: body(n), slot: SLOT_CELL }),
            NodeKind::Block(_) => v.push(Inst { r: body(n), slot: SLOT_CELL }),
            _ => {}
        }
    }
    ranges[SLOT_CELL as usize] = (a, v.len() as u32);
    let a = v.len() as u32;
    for n in &lay.nodes {
        if let NodeKind::InPort(_) | NodeKind::OutPort(_) | NodeKind::InStub(_) | NodeKind::OutStub(_) = n.kind {
            v.push(Inst { r: gpu_rect(n.pos, n.pos + Vec2::new(n.w, n.h)), slot: SLOT_PORT });
        }
        if let NodeKind::Frame(_) = n.kind {
            let (p, q, t) = (n.pos, n.pos + Vec2::new(n.w, n.h), 1.5);
            for (a, b) in [(p, Pos2::new(q.x, p.y + t)), (Pos2::new(p.x, q.y - t), q), (p, Pos2::new(p.x + t, q.y)), (Pos2::new(q.x - t, p.y), q)] {
                v.push(Inst { r: gpu_rect(a, b), slot: SLOT_PORT });
            }
        }
    }
    for w in lay.wires.iter().filter(|w| w.bundle) {
        for s in w.pts.windows(2) {
            v.push(Inst { r: gpu_rect(s[0].min(s[1]) - Vec2::splat(1.5), s[0].max(s[1]) + Vec2::splat(1.5)), slot: SLOT_PORT });
        }
    }
    ranges[SLOT_PORT as usize] = (a, v.len() as u32);
    let a = v.len() as u32;
    for n in &lay.nodes {
        if let NodeKind::Cell(ci) = n.kind {
            if is_seq(ci) {
                v.push(Inst { r: body(n), slot: SLOT_SEQ });
            }
        }
    }
    ranges[SLOT_SEQ as usize] = (a, v.len() as u32);
    (v, ranges)
}

/// highlight instances for the selection: the wires of a net (about 3 px wide at this zoom), or the box
/// of a cell
pub fn gpu_hl(lay: &SchLayout, sel: Option<Hit>, zoom: f32) -> Vec<crate::scene::Inst> {
    use crate::scene::Inst;
    let mut v = vec![];
    let half = 1.5 / zoom.max(1e-6);
    match sel {
        Some(Hit::Net(_)) | Some(Hit::Bundle(_)) => {
            let reps = bundles_shown(lay, sel);
            for w in lay.wires.iter().filter(|w| if w.bundle { reps.contains(&w.net) } else { sel == Some(Hit::Net(w.net)) }) {
                for s in w.pts.windows(2) {
                    let (a, b) = (s[0].min(s[1]), s[0].max(s[1]));
                    v.push(Inst { r: gpu_rect(a - Vec2::splat(half), b + Vec2::splat(half)), slot: SLOT_HL_LINE });
                }
            }
        }
        Some(Hit::Cell(ci)) => {
            if let Some(&ni) = lay.cell_node.get(&ci) {
                let n = &lay.nodes[ni];
                v.push(Inst { r: gpu_rect(n.pos + Vec2::new(4.0, -4.0), n.pos + Vec2::new(n.w - 4.0, n.h + 4.0)), slot: SLOT_HL_BOX });
            }
        }
        Some(Hit::Group(g)) => {
            if let Some(&ni) = lay.group_node.get(&g) {
                let n = &lay.nodes[ni];
                v.push(Inst { r: gpu_rect(n.pos + Vec2::new(4.0, -4.0), n.pos + Vec2::new(n.w - 4.0, n.h + 4.0)), slot: SLOT_HL_BOX });
            }
        }
        None => {}
    }
    v
}

/// styles for the slots above: faint wires so the cells show, logic cells in ink, ports in the wire
/// colour, flip-flops and latches in blue, the selected net in the accent colour
pub fn gpu_styles(col: &Colors) -> Vec<crate::gpu::Style> {
    use crate::gpu::Style;
    let c = |c: Color32| [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0];
    vec![
        Style { color: c(col.wire), p: [0.14, 0.0, 4.0, 0.0] },
        Style { color: c(col.ink), p: [0.25, 0.0, 0.0, 0.0] },
        Style { color: c(col.wire), p: [0.3, 0.0, 0.0, 0.0] },
        Style { color: c(col.sel), p: [0.35, 0.0, 0.0, 0.0] },
        Style { color: c(col.hl), p: [1.0, 0.0, 4.0, 0.0] },
        Style { color: c(col.hl), p: [0.0, 0.0, 2.0, 0.0] },
    ]
}

/// camera for the overview: the view's centre, pixels per integer unit
pub fn gpu_cam(view: &View, rect: Rect, dpr: f32) -> crate::gpu::Cam {
    let c = view.pan + rect.size() / (2.0 * view.zoom);
    crate::gpu::Cam { origin: [(c.x * GPU_SCALE).round() as i32, (-c.y * GPU_SCALE).round() as i32], size: [rect.width() * dpr, rect.height() * dpr], scale: view.zoom * dpr / GPU_SCALE, dpr, pad: [0.0; 2] }
}

/// number of cells inside the view (decides between the overview and the detailed drawing)
pub fn cells_in_view(lay: &SchLayout, view: &View, rect: Rect) -> usize {
    let w = Rect::from_min_size(view.pan.to_pos2(), rect.size() / view.zoom);
    lay.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Cell(_) | NodeKind::Block(_)) && w.intersects(Rect::from_min_size(n.pos, Vec2::new(n.w, n.h)))).count()
}

// ---------- drawing ----------

pub struct Colors {
    pub ink: Color32,
    pub wire: Color32,
    pub muted: Color32,
    pub hl: Color32,
    pub sel: Color32,
    pub fill: Color32,
}

fn arc(c: Pos2, r: f32, a0: f32, a1: f32, n: usize) -> Vec<Pos2> {
    (0..=n).map(|i| {
        let a = a0 + (a1 - a0) * i as f32 / n as f32;
        c + Vec2::new(a.cos() * r, a.sin() * r)
    })
    .collect()
}
fn quad(a: Pos2, c: Pos2, b: Pos2, n: usize) -> Vec<Pos2> {
    (0..=n).map(|i| {
        let t = i as f32 / n as f32;
        let u = 1.0 - t;
        Pos2::new(u * u * a.x + 2.0 * u * t * c.x + t * t * b.x, u * u * a.y + 2.0 * u * t * c.y + t * t * b.y)
    })
    .collect()
}

/// gate outline in screen space for a rectangle (x, y, w, h); returns the shape points (closed)
fn gate_outline(gt: GT, r: Rect) -> Vec<Pos2> {
    let (x, y, w, h) = (r.min.x, r.min.y, r.width(), r.height());
    match gt {
        GT::And => {
            let mut p = vec![Pos2::new(x, y), Pos2::new(x + w - h / 2.0, y)];
            p.extend(arc(Pos2::new(x + w - h / 2.0, y + h / 2.0), h / 2.0, -std::f32::consts::FRAC_PI_2, std::f32::consts::FRAC_PI_2, 16));
            p.push(Pos2::new(x, y + h));
            p
        }
        GT::Or | GT::Xor => {
            let mut p = quad(Pos2::new(x, y), Pos2::new(x + w * 0.6, y), Pos2::new(x + w, y + h / 2.0), 12);
            p.extend(quad(Pos2::new(x + w, y + h / 2.0), Pos2::new(x + w * 0.6, y + h), Pos2::new(x, y + h), 12));
            p.extend(quad(Pos2::new(x, y + h), Pos2::new(x + w * 0.25, y + h / 2.0), Pos2::new(x, y), 10));
            p
        }
        GT::Buf => vec![Pos2::new(x, y), Pos2::new(x + w, y + h / 2.0), Pos2::new(x, y + h)],
        GT::Mux => vec![Pos2::new(x, y), Pos2::new(x + w, y + h * 0.2), Pos2::new(x + w, y + h * 0.8), Pos2::new(x, y + h)],
        _ => vec![Pos2::new(x, y), Pos2::new(x + w, y), Pos2::new(x + w, y + h), Pos2::new(x, y + h)],
    }
}

pub struct View {
    pub pan: Vec2,
    pub zoom: f32,
}

impl View {
    pub fn to_screen(&self, rect: Rect, p: Pos2) -> Pos2 {
        rect.min + (p.to_vec2() - self.pan) * self.zoom
    }
    pub fn to_world(&self, rect: Rect, s: Pos2) -> Pos2 {
        ((s - rect.min) / self.zoom + self.pan).to_pos2()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Hit {
    Cell(u32),
    Net(u32),
    /// a submodule box or the title band of its frame
    Group(u32),
    /// a bundle wire, or a port or stub standing for several nets (by the bundle's first net)
    Bundle(u32),
}

/// bundle wires to highlight: the selected bundle, or the bundles carrying the selected net
pub fn bundles_shown(lay: &SchLayout, sel: Option<Hit>) -> HashSet<u32> {
    match sel {
        Some(Hit::Bundle(r)) => std::iter::once(r).collect(),
        Some(Hit::Net(n)) => lay.bundles.iter().filter(|b| b.at.is_some() && b.nets.contains(&n)).map(|b| b.rep).collect(),
        _ => HashSet::new(),
    }
}

pub fn hit_test(lay: &SchLayout, p: Pos2, tol: f32) -> Option<Hit> {
    for n in &lay.nodes {
        if let NodeKind::Cell(ci) = n.kind {
            if Rect::from_min_size(n.pos, Vec2::new(n.w, n.h)).expand(2.0).contains(p) {
                return Some(Hit::Cell(ci));
            }
        }
    }
    for &i in lay.group_node.values() {
        let n = &lay.nodes[i];
        match n.kind {
            NodeKind::Block(g) if Rect::from_min_size(n.pos + Vec2::new(8.0, 0.0), Vec2::new(n.w - 16.0, n.h)).contains(p) => return Some(Hit::Group(g)),
            NodeKind::Frame(g) if Rect::from_min_size(n.pos, Vec2::new(n.w, FRAME_HEAD)).contains(p) => return Some(Hit::Group(g)),
            _ => {}
        }
    }
    let mut best: Option<(f32, Hit)> = None;
    for w in &lay.wires {
        for s in w.pts.windows(2) {
            let (a, b) = (s[0], s[1]);
            let ab = b - a;
            let t = if ab.length_sq() > 0.0 { ((p - a).dot(ab) / ab.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
            let d = (a + ab * t - p).length();
            if d < tol && best.map(|x| d < x.0).unwrap_or(true) {
                best = Some((d, if w.bundle { Hit::Bundle(w.net) } else { Hit::Net(w.net) }));
            }
        }
    }
    for n in &lay.nodes {
        if let NodeKind::InPort(_) | NodeKind::OutPort(_) | NodeKind::InStub(_) | NodeKind::OutStub(_) = n.kind {
            if Rect::from_min_size(n.pos, Vec2::new(n.w, n.h)).contains(p) {
                let net = n.outs.first().or(n.ins.first()).map(|x| x.0).unwrap();
                return Some(if n.ins.len() + n.outs.len() > 1 { Hit::Bundle(net) } else { Hit::Net(net) });
            }
        }
    }
    best.map(|b| b.1)
}


/// drawing target for the schematic: the egui painter on screen, or an SVG document
pub trait Canvas {
    fn line(&mut self, pts: Vec<Pos2>, st: Stroke);
    fn closed(&mut self, pts: Vec<Pos2>, fill: Option<Color32>, st: Stroke);
    fn circle(&mut self, c: Pos2, r: f32, fill: Option<Color32>, st: Stroke);
    fn circle_stroke(&mut self, c: Pos2, r: f32, st: Stroke) {
        self.circle(c, r, None, st)
    }
    fn text(&mut self, p: Pos2, a: egui::Align2, s: &str, size: f32, c: Color32);
    fn dashed(&mut self, pts: Vec<Pos2>, st: Stroke);
}

impl Canvas for egui::Painter {
    fn line(&mut self, pts: Vec<Pos2>, st: Stroke) {
        self.add(Shape::line(pts, st));
    }
    fn closed(&mut self, pts: Vec<Pos2>, fill: Option<Color32>, st: Stroke) {
        if let Some(f) = fill {
            self.add(Shape::convex_polygon(pts.clone(), f, Stroke::NONE));
        }
        if st.width > 0.0 {
            self.add(Shape::closed_line(pts, st));
        }
    }
    fn circle(&mut self, c: Pos2, r: f32, fill: Option<Color32>, st: Stroke) {
        match fill {
            Some(f) => {
                self.circle_filled(c, r, f);
            }
            None => {
                egui::Painter::circle_stroke(self, c, r, st);
            }
        }
    }
    fn text(&mut self, p: Pos2, a: egui::Align2, s: &str, size: f32, c: Color32) {
        egui::Painter::text(self, p, a, s, egui::FontId::monospace(size), c);
    }
    fn dashed(&mut self, pts: Vec<Pos2>, st: Stroke) {
        self.extend(Shape::dashed_line(&pts, st, 3.0, 2.0));
    }
}

/// SVG writer with the same drawing calls
pub struct Svg {
    pub body: String,
}
fn hex(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    if a == 255 {
        format!("#{:02x}{:02x}{:02x}", r, g, b)
    } else {
        format!("rgba({},{},{},{:.3})", r, g, b, a as f32 / 255.0)
    }
}
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
fn svg_path(pts: &[Pos2]) -> String {
    pts.iter().enumerate().map(|(i, p)| format!("{}{:.1},{:.1}", if i == 0 { "M" } else { "L" }, p.x, p.y)).collect::<Vec<_>>().join("")
}
impl Canvas for Svg {
    fn line(&mut self, pts: Vec<Pos2>, st: Stroke) {
        self.body += &format!(r#"<path d="{}" fill="none" stroke="{}" stroke-width="{:.2}"/>"#, svg_path(&pts), hex(st.color), st.width);
    }
    fn closed(&mut self, pts: Vec<Pos2>, fill: Option<Color32>, st: Stroke) {
        let f = fill.map(hex).unwrap_or_else(|| "none".into());
        let s = if st.width > 0.0 { format!(r#" stroke="{}" stroke-width="{:.2}""#, hex(st.color), st.width) } else { String::new() };
        self.body += &format!(r#"<path d="{}Z" fill="{}"{}/>"#, svg_path(&pts), f, s);
    }
    fn circle(&mut self, c: Pos2, r: f32, fill: Option<Color32>, st: Stroke) {
        match fill {
            Some(f) => self.body += &format!(r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="{}"/>"#, c.x, c.y, r, hex(f)),
            None => self.body += &format!(r#"<circle cx="{:.1}" cy="{:.1}" r="{:.1}" fill="none" stroke="{}" stroke-width="{:.2}"/>"#, c.x, c.y, r, hex(st.color), st.width),
        }
    }
    fn text(&mut self, p: Pos2, a: egui::Align2, s: &str, size: f32, c: Color32) {
        let anchor = match a.x() {
            egui::Align::Min => "start",
            egui::Align::Center => "middle",
            egui::Align::Max => "end",
        };
        let dy = match a.y() {
            egui::Align::Min => size * 0.8,
            egui::Align::Center => size * 0.35,
            egui::Align::Max => 0.0,
        };
        self.body += &format!(r#"<text x="{:.1}" y="{:.1}" font-family="monospace" font-size="{:.1}" text-anchor="{}" fill="{}">{}</text>"#, p.x, p.y + dy, size, anchor, hex(c), esc(s));
    }
    fn dashed(&mut self, pts: Vec<Pos2>, st: Stroke) {
        self.body += &format!(r#"<path d="{}" fill="none" stroke="{}" stroke-width="{:.2}" stroke-dasharray="3 2"/>"#, svg_path(&pts), hex(st.color), st.width);
    }
}

/// the whole schematic as a standalone SVG document
pub fn to_svg(lay: &SchLayout, nl: &Netlist, sel: Option<Hit>, col: &Colors) -> String {
    let mut svg = Svg { body: String::new() };
    let rect = Rect::from_min_size(Pos2::ZERO, lay.size);
    let view = View { pan: Vec2::ZERO, zoom: 1.0 };
    paint(&mut svg, rect, &view, lay, nl, sel, col);
    format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{:.0}" height="{:.0}" viewBox="0 0 {:.0} {:.0}"><rect width="100%" height="100%" fill="#ffffff"/>{}</svg>"##, lay.size.x, lay.size.y, lay.size.x, lay.size.y, svg.body)
}

pub fn paint(cv: &mut dyn Canvas, rect: Rect, view: &View, lay: &SchLayout, nl: &Netlist, sel: Option<Hit>, col: &Colors) {
    let z = view.zoom;
    let s = |p: Pos2| view.to_screen(rect, p);
    let sel_net = match sel {
        Some(Hit::Net(n)) => Some(n),
        _ => None,
    };
    // only what is inside the view (plus a margin) is drawn
    let world = Rect::from_min_size(view.pan.to_pos2(), rect.size() / z).expand(40.0 / z.min(1.0));
    // frames of opened submodules first, outer ones before the frames inside them
    for n in &lay.nodes {
        if let NodeKind::Frame(g) = n.kind {
            let r = Rect::from_min_size(s(n.pos), Vec2::new(n.w, n.h) * z);
            if !rect.expand(200.0).intersects(r) {
                continue;
            }
            let on = sel == Some(Hit::Group(g));
            let st = Stroke::new(if on { 2.2 } else { 1.3 }, if on { col.sel } else { col.muted });
            cv.closed(vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()], None, st);
            let y = r.min.y + FRAME_HEAD * z;
            cv.line(vec![Pos2::new(r.min.x, y), Pos2::new(r.max.x, y)], Stroke::new(1.0, col.muted));
        }
    }
    let reps = bundles_shown(lay, sel);
    for w in &lay.wires {
        let (mut a, mut b) = (w.pts[0], w.pts[0]);
        for p in &w.pts {
            a = a.min(*p);
            b = b.max(*p);
        }
        if !world.intersects(Rect::from_min_max(a, b)) {
            continue;
        }
        let on = if w.bundle { reps.contains(&w.net) } else { sel_net == Some(w.net) };
        let width = match (w.bundle, on) {
            (false, false) => 1.2,
            (false, true) => 2.6,
            (true, false) => 3.0,
            (true, true) => 4.0,
        };
        let st = Stroke::new(width, if on { col.hl } else { col.wire });
        cv.line(w.pts.iter().map(|&p| s(p)).collect(), st);
    }
    for &(net, p) in &lay.dots {
        if !world.contains(p) {
            continue;
        }
        cv.circle(s(p), 2.6 * z.max(0.6), Some(if sel_net == Some(net) { col.hl } else { col.wire }), Stroke::NONE);
    }
    let font = (9.0 * z).clamp(6.0, 18.0);
    let small = (7.0 * z).clamp(5.0, 14.0);
    for n in &lay.nodes {
        let r = Rect::from_min_size(s(n.pos), Vec2::new(n.w, n.h) * z);
        if !rect.expand(200.0).intersects(r) {
            continue;
        }
        match &n.kind {
            NodeKind::Cell(ci) => {
                let c = &nl.cells[*ci as usize];
                let m = &c.model;
                let selected = sel == Some(Hit::Cell(*ci));
                let st = Stroke::new(if selected { 2.2 } else { 1.3 }, if selected { col.sel } else { col.ink });
                let body = Rect::from_min_max(r.min + Vec2::new(8.0 * z, 0.0), r.max - Vec2::new(8.0 * z, 0.0));
                let gate = if m.kind == Kind::Gate { m.out_gates.values().next() } else { None };
                match gate {
                    Some(g) => {
                        let pts = gate_outline(g.gate, body);
                        if !matches!(g.gate, GT::Or | GT::Xor) {
                            cv.closed(pts.clone(), Some(col.fill), Stroke::NONE);
                        }
                        cv.closed(pts, None, st);
                        if g.gate == GT::Xor {
                            let back = quad(Pos2::new(body.min.x - 5.0 * z, body.max.y), Pos2::new(body.min.x + body.width() * 0.25 - 5.0 * z, body.center().y), Pos2::new(body.min.x - 5.0 * z, body.min.y), 10);
                            cv.line(back, st);
                        }
                        if matches!(g.gate, GT::Maj) {
                            cv.text(body.center(), egui::Align2::CENTER_CENTER, "MAJ", small, col.ink);
                        }
                        // output bubble
                        let out_tip = Pos2::new(body.max.x, body.center().y);
                        if g.out {
                            cv.circle_stroke(out_tip + Vec2::new(3.0 * z, 0.0), 3.0 * z, st);
                        }
                        // input pins and bubbles, in the gate's own input order
                        let src_pins: Vec<(&str, bool)> = g.ins.iter().filter_map(|x| if let Src::Pin(p, b) = x { Some((p.as_str(), *b)) } else { None }).collect();
                        for (i, (pin, (_, y))) in n.pin_names_in.iter().zip(n.ins.iter()).enumerate() {
                            let py = r.min.y + y * z;
                            let bub = src_pins.iter().find(|x| x.0 == pin).map(|x| x.1).unwrap_or(false);
                            let edge_x = body.min.x + if matches!(g.gate, GT::Or | GT::Xor) { body.width() * 0.12 } else { 0.0 };
                            cv.line(vec![Pos2::new(r.min.x, py), Pos2::new(edge_x, py)], Stroke::new(1.2, col.wire));
                            if bub {
                                cv.circle_stroke(Pos2::new(edge_x - 3.0 * z, py), 3.0 * z, st);
                            }
                            let _ = i;
                        }
                        for (_, y) in &n.outs {
                            let py = r.min.y + y * z;
                            let x0 = out_tip.x + if g.out { 6.0 * z } else { 0.0 };
                            cv.line(vec![Pos2::new(x0, py.min(out_tip.y).max(out_tip.y)), Pos2::new(r.max.x, py)], Stroke::new(1.2, col.wire));
                        }
                    }
                    None => {
                        cv.closed(vec![body.left_top(), body.right_top(), body.right_bottom(), body.left_bottom()], Some(col.fill), st);
                        let title = if m.kind == Kind::Seq { format!("{}  {}", m.title, m.kind_text) } else { m.title.clone() };
                        cv.text(Pos2::new(body.center().x, body.min.y + 3.0 * z), egui::Align2::CENTER_TOP, &title, small, col.ink);
                        for (pin, (_, y)) in n.pin_names_in.iter().zip(n.ins.iter()) {
                            let py = r.min.y + y * z;
                            cv.line(vec![Pos2::new(r.min.x, py), Pos2::new(body.min.x, py)], Stroke::new(1.2, col.wire));
                            if z > 0.7 {
                                cv.text(Pos2::new(body.min.x + 2.0 * z, py), egui::Align2::LEFT_CENTER, pin, small, col.muted);
                            }
                            if m.clocks.contains(pin) {
                                cv.line(vec![Pos2::new(body.min.x, py - 3.0 * z), Pos2::new(body.min.x + 5.0 * z, py), Pos2::new(body.min.x, py + 3.0 * z)], st);
                            }
                        }
                        for (pin, (_, y)) in n.pin_names_out.iter().zip(n.outs.iter()) {
                            let py = r.min.y + y * z;
                            cv.line(vec![Pos2::new(body.max.x, py), Pos2::new(r.max.x, py)], Stroke::new(1.2, col.wire));
                            if z > 0.7 {
                                cv.text(Pos2::new(body.max.x - 2.0 * z, py), egui::Align2::RIGHT_CENTER, pin, small, col.muted);
                            }
                        }
                    }
                }
                if z > 0.45 {
                    cv.text(Pos2::new(r.center().x, r.min.y - 2.0 * z), egui::Align2::CENTER_BOTTOM, &short(&c.name), small, col.muted);
                }
            }
            NodeKind::InPort(_) | NodeKind::OutPort(_) | NodeKind::InStub(_) | NodeKind::OutStub(_) => {
                let net = n.outs.first().or(n.ins.first()).map(|x| x.0).unwrap();
                let on = sel_net == Some(net) || sel == Some(Hit::Bundle(net));
                let port = matches!(n.kind, NodeKind::InPort(_) | NodeKind::OutPort(_));
                let st = Stroke::new(if on { 2.0 } else { 1.0 }, if on { col.hl } else { col.wire });
                let pts = vec![r.left_top(), Pos2::new(r.max.x - 5.0 * z, r.min.y), Pos2::new(r.max.x, r.center().y), Pos2::new(r.max.x - 5.0 * z, r.max.y), r.left_bottom()];
                if port {
                    cv.closed(pts.clone(), Some(col.fill), Stroke::NONE);
                    cv.closed(pts, None, st);
                } else {
                    cv.dashed(vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()], st);
                }
                // a port or stub for several nets carries their bus label
                let label = n.pin_names_out.first().or(n.pin_names_in.first()).map(|x| x.as_str()).unwrap_or(nl.nets[net as usize].name.as_str());
                cv.text(Pos2::new(r.min.x + 4.0 * z, r.center().y), egui::Align2::LEFT_CENTER, label, font, if on { col.hl } else { col.ink });
            }
            NodeKind::Block(g) => {
                let on = sel == Some(Hit::Group(*g));
                let st = Stroke::new(if on { 2.4 } else { 1.6 }, if on { col.sel } else { col.ink });
                let body = Rect::from_min_max(r.min + Vec2::new(8.0 * z, 0.0), r.max - Vec2::new(8.0 * z, 0.0));
                cv.closed(vec![body.left_top(), body.right_top(), body.right_bottom(), body.left_bottom()], Some(col.fill), st);
                let y = body.min.y + BOX_HEAD * z;
                cv.line(vec![Pos2::new(body.min.x, y), Pos2::new(body.max.x, y)], Stroke::new(1.0, col.muted));
                let mut last = f32::NAN;
                for (pin, (_, y)) in n.pin_names_in.iter().zip(n.ins.iter()) {
                    if *y == last {
                        continue;
                    }
                    last = *y;
                    let py = r.min.y + y * z;
                    cv.line(vec![Pos2::new(r.min.x, py), Pos2::new(body.min.x, py)], Stroke::new(1.2, col.wire));
                    if z > 0.7 {
                        cv.text(Pos2::new(body.min.x + 2.0 * z, py), egui::Align2::LEFT_CENTER, &short_n(pin, 24), small, col.muted);
                    }
                }
                let mut last = f32::NAN;
                for (pin, (_, y)) in n.pin_names_out.iter().zip(n.outs.iter()) {
                    if *y == last {
                        continue;
                    }
                    last = *y;
                    let py = r.min.y + y * z;
                    cv.line(vec![Pos2::new(body.max.x, py), Pos2::new(r.max.x, py)], Stroke::new(1.2, col.wire));
                    if z > 0.7 {
                        cv.text(Pos2::new(body.max.x - 2.0 * z, py), egui::Align2::RIGHT_CENTER, &short_n(pin, 24), small, col.muted);
                    }
                }
            }
            NodeKind::Dummy | NodeKind::Frame(_) => {}
        }
    }
    paint_marks(cv, rect, view, lay, sel, col, false);
}

/// titles and +/- buttons of the submodule boxes and frames. Over the GPU overview (`bodies`) also the box
/// bodies and frame outlines, so the submodules stand out from the wires
pub fn paint_marks(cv: &mut dyn Canvas, rect: Rect, view: &View, lay: &SchLayout, sel: Option<Hit>, col: &Colors, bodies: bool) {
    let z = view.zoom;
    // bundle labels where their wire piece is long enough on screen
    let reps = bundles_shown(lay, sel);
    for b in &lay.bundles {
        if let Some(at) = b.at {
            let p = view.to_screen(rect, at);
            let size = (9.0 * z).clamp(9.0, 13.0);
            if !rect.contains(p) || b.len * z < b.label.chars().count() as f32 * size * 0.62 + 8.0 {
                continue;
            }
            cv.text(p - Vec2::new(0.0, 2.5), egui::Align2::CENTER_BOTTOM, &b.label, size, if reps.contains(&b.rep) { col.hl } else { col.ink });
        }
    }
    {
        for &i in lay.group_node.values() {
            let n = &lay.nodes[i];
            let r = Rect::from_min_size(view.to_screen(rect, n.pos), Vec2::new(n.w, n.h) * z);
            if !rect.intersects(r) || r.width().max(r.height()) < 3.0 {
                continue;
            }
            match n.kind {
                _ if !bodies => {}
                NodeKind::Block(g) => {
                    let on = sel == Some(Hit::Group(g));
                    let b = Rect::from_min_max(r.min + Vec2::new(8.0 * z, 0.0), r.max - Vec2::new(8.0 * z, 0.0));
                    cv.closed(vec![b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom()], Some(col.fill), Stroke::new(if on { 2.4 } else { 1.4 }, if on { col.sel } else { col.ink }));
                }
                NodeKind::Frame(g) => {
                    let on = sel == Some(Hit::Group(g));
                    cv.closed(vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()], None, Stroke::new(if on { 2.2 } else { 1.3 }, if on { col.sel } else { col.muted }));
                }
                _ => {}
            }
            // a box too narrow for its own title gets a name tag above it
            if let (NodeKind::Block(g), true) = (&n.kind, r.width() < 40.0 && r.height() >= 4.0) {
                if let Some(t) = lay.labels.get(g) {
                    let txt = short_n(&t.title, 24);
                    let w = txt.chars().count() as f32 * 6.4 + 8.0;
                    let c = Pos2::new(r.center().x, r.min.y - 9.0);
                    let tag = Rect::from_center_size(c, Vec2::new(w, 14.0));
                    cv.closed(vec![tag.left_top(), tag.right_top(), tag.right_bottom(), tag.left_bottom()], Some(col.fill), Stroke::new(1.0, col.muted));
                    cv.text(c, egui::Align2::CENTER_CENTER, &txt, 10.5, col.ink);
                }
            }
        }
    }
    for &i in lay.group_node.values() {
        let n = &lay.nodes[i];
        let r = Rect::from_min_size(view.to_screen(rect, n.pos), Vec2::new(n.w, n.h) * z);
        if !rect.intersects(r) || r.width() < 40.0 {
            continue;
        }
        let (g, open) = match n.kind {
            NodeKind::Block(g) => (g, false),
            NodeKind::Frame(g) => (g, true),
            _ => continue,
        };
        let t = match lay.labels.get(&g) {
            Some(t) => t,
            None => continue,
        };
        let font = (10.0 * z).clamp(9.0, 16.0);
        let b = fold_button(n, view, rect);
        if open {
            let x = b.map(|b| b.max.x + 6.0).unwrap_or(r.min.x + 6.0);
            let title = if t.title == t.path { t.path.clone() } else { format!("{}  ·  {}", t.title, t.path) };
            if r.height() > 14.0 {
                cv.text(Pos2::new(x, r.min.y + (FRAME_HEAD * z / 2.0).max(9.0)), egui::Align2::LEFT_CENTER, &title, font, col.ink);
            }
        } else {
            // title between the left edge and the + button
            let (x0, x1) = (r.min.x + 8.0 * z, b.map(|b| b.min.x).unwrap_or(r.max.x - 8.0 * z));
            let chars = ((x1 - x0 - 6.0) / (font * 0.62)).max(4.0) as usize;
            cv.text(Pos2::new((x0 + x1) / 2.0, r.min.y + 3.0 * z.min(1.0) + 1.0), egui::Align2::CENTER_TOP, &short_n(&t.title, chars), font, col.ink);
            if r.height() > 3.0 * font {
                cv.text(Pos2::new(r.center().x, r.min.y + 3.0 * z.min(1.0) + font + 3.0), egui::Align2::CENTER_TOP, &crate::trf!("{} cells", t.cells), font * 0.8, col.muted);
            }
            if z > 0.45 {
                cv.text(Pos2::new(r.center().x, r.min.y - 2.0 * z), egui::Align2::CENTER_BOTTOM, &short(&t.path), (7.0 * z).clamp(5.0, 14.0), col.muted);
            }
        }
        if let Some(b) = b {
            let st = Stroke::new(1.3, col.ink);
            cv.closed(vec![b.left_top(), b.right_top(), b.right_bottom(), b.left_bottom()], Some(col.fill), st);
            let c = b.center();
            let h = b.width() * 0.28;
            cv.line(vec![Pos2::new(c.x - h, c.y), Pos2::new(c.x + h, c.y)], st);
            if !open {
                cv.line(vec![Pos2::new(c.x, c.y - h), Pos2::new(c.x, c.y + h)], st);
            }
        }
    }
}

fn short_n(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n > max && max > 2 {
        format!("…{}", s.chars().skip(n + 1 - max).collect::<String>())
    } else {
        s.to_string()
    }
}

fn short(s: &str) -> String {
    let n = s.chars().count();
    if n > 28 {
        format!("…{}", s.chars().skip(n - 26).collect::<String>())
    } else {
        s.to_string()
    }
}
