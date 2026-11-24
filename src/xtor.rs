//! Transistor-level schematic of one CDL subcircuit (port of xtor.js). MOSFETs with source/drain wiring
//! drawn as wires, gates as net labels, VDD/VSS as supply symbols. The web version uses ELK; here a
//! small layered layout runs top to bottom: cycle breaking, longest-path layers, dummies for long edges,
//! barycenter ordering, port-aligned priority x placement and one horizontal track per net in each channel.
use crate::cdl::{Mos, Subckt};
use crate::schematic::{Canvas, Colors, View};
use eframe::egui::{self, Pos2, Rect, Stroke, Vec2};
use std::collections::{HashMap, HashSet};

const DW: f32 = 40.0;
const DH: f32 = 48.0;
const NODE_SEP: f32 = 26.0;
const DUMMY_SEP: f32 = 10.0;
const LAYER_GAP: f32 = 34.0;
const TRACK: f32 = 7.0;

pub fn text_w(s: &str) -> f32 {
    6.2 * s.chars().count() as f32 + 4.0
}

#[derive(Clone, Debug, Default)]
pub struct Supplies {
    pub vdd: Option<String>,
    pub vss: Option<String>,
}

pub fn supplies(sc: &Subckt) -> Supplies {
    let count = |typ: &str| -> Option<String> {
        let mut order: Vec<String> = vec![];
        let mut m: HashMap<&str, usize> = HashMap::new();
        for d in sc.mos.iter().filter(|d| d.typ == typ) {
            if !m.contains_key(d.b.as_str()) {
                order.push(d.b.clone());
            }
            *m.entry(&d.b).or_insert(0) += 1;
        }
        // stable sort by count, descending (first seen wins ties, like Array.sort in JS)
        order.sort_by(|a, b| m[b.as_str()].cmp(&m[a.as_str()]));
        order.into_iter().next()
    };
    let by_name = |names: &[&str]| sc.ports.iter().find(|x| names.iter().any(|n| x.eq_ignore_ascii_case(n))).cloned();
    Supplies { vdd: count("pmos").or_else(|| by_name(&["vdd", "vcc", "vpwr", "vdd!"])), vss: count("nmos").or_else(|| by_name(&["vss", "gnd", "vgnd", "vss!", "0"])) }
}

/// PMOS terminal nearer VDD on top, NMOS terminal nearer VSS at the bottom: (top, bottom) per device
fn orient(sc: &Subckt, sup: &Supplies) -> Vec<(String, String)> {
    let dist = |typ: &str, root: &str| -> HashMap<String, usize> {
        let mut d = HashMap::new();
        d.insert(root.to_string(), 0usize);
        let mut q = std::collections::VecDeque::from([root.to_string()]);
        while let Some(n) = q.pop_front() {
            for m in sc.mos.iter().filter(|m| m.typ == typ) {
                let o = if m.d == n { Some(&m.s) } else if m.s == n { Some(&m.d) } else { None };
                if let Some(o) = o {
                    if !d.contains_key(o) {
                        let v = d[&n] + 1;
                        d.insert(o.clone(), v);
                        q.push_back(o.clone());
                    }
                }
            }
        }
        d
    };
    let dp = sup.vdd.as_ref().map(|v| dist("pmos", v)).unwrap_or_default();
    let dn = sup.vss.as_ref().map(|v| dist("nmos", v)).unwrap_or_default();
    sc.mos
        .iter()
        .map(|m| {
            let dd = if m.typ == "pmos" { &dp } else { &dn };
            let a = dd.get(&m.s).copied().unwrap_or(usize::MAX);
            let b = dd.get(&m.d).copied().unwrap_or(usize::MAX);
            if m.typ == "pmos" {
                if a <= b { (m.s.clone(), m.d.clone()) } else { (m.d.clone(), m.s.clone()) }
            } else if a <= b {
                (m.d.clone(), m.s.clone())
            } else {
                (m.s.clone(), m.d.clone())
            }
        })
        .collect()
}

#[derive(Clone, Debug)]
pub enum XKind {
    Dev(usize),
    Junction { net: String, port: bool },
    Dummy,
}

#[derive(Clone, Debug)]
pub struct XNode {
    pub kind: XKind,
    pub w: f32,
    pub h: f32,
    pub gw: f32,
    pub pos: Pos2,
    pub layer: usize,
}

impl XNode {
    /// x offset of the top/bottom port inside the node
    fn port_dx(&self) -> f32 {
        match self.kind {
            XKind::Dev(_) => self.gw + 28.0,
            XKind::Junction { .. } => 4.0,
            XKind::Dummy => 0.0,
        }
    }
    fn top_port(&self) -> Pos2 {
        match self.kind {
            XKind::Junction { .. } => Pos2::new(self.pos.x + 4.0, self.pos.y + 4.0),
            _ => Pos2::new(self.pos.x + self.port_dx(), self.pos.y),
        }
    }
    fn bot_port(&self) -> Pos2 {
        match self.kind {
            XKind::Junction { .. } => Pos2::new(self.pos.x + 4.0, self.pos.y + 4.0),
            _ => Pos2::new(self.pos.x + self.port_dx(), self.pos.y + self.h),
        }
    }
}

#[derive(Clone, Debug)]
pub struct XWire {
    pub net: String,
    pub pts: Vec<Pos2>,
}

#[derive(Clone, Debug, Default)]
pub struct XLayout {
    pub nodes: Vec<XNode>,
    pub wires: Vec<XWire>,
    pub orient: Vec<(String, String)>,
    pub sup: Supplies,
    pub inputs: Vec<String>,
    pub size: Vec2,
}

pub fn layout(sc: &Subckt) -> XLayout {
    let sup = supplies(sc);
    let or = orient(sc, &sup);
    let ports: HashSet<&String> = sc.ports.iter().collect();
    let mut nodes: Vec<XNode> = vec![];
    // edges: (from node, to node, net); from = bottom terminal side, to = top terminal side
    let mut edges: Vec<(usize, usize, String)> = vec![];
    let mut term_order: Vec<String> = vec![];
    let mut terms: HashMap<String, Vec<(usize, bool)>> = HashMap::new(); // net -> (node, is_top)
    let is_sup = |n: &str| Some(n) == sup.vdd.as_deref() || Some(n) == sup.vss.as_deref();
    for (i, m) in sc.mos.iter().enumerate() {
        let gw = text_w(&m.g).max(18.0);
        nodes.push(XNode { kind: XKind::Dev(i), w: gw + DW, h: DH, gw, pos: Pos2::ZERO, layer: 0 });
        for (net, top) in [(&or[i].0, true), (&or[i].1, false)] {
            if is_sup(net) {
                continue;
            }
            if !terms.contains_key(net) {
                term_order.push(net.clone());
            }
            terms.entry(net.clone()).or_default().push((i, top));
        }
    }
    for net in &term_order {
        let list = &terms[net];
        let tops: Vec<usize> = list.iter().filter(|t| t.1).map(|t| t.0).collect();
        let bots: Vec<usize> = list.iter().filter(|t| !t.1).map(|t| t.0).collect();
        if list.len() == 2 && tops.len() == 1 && bots.len() == 1 && !ports.contains(net) {
            edges.push((bots[0], tops[0], net.clone()));
            continue;
        }
        let j = nodes.len();
        nodes.push(XNode { kind: XKind::Junction { net: net.clone(), port: ports.contains(net) }, w: 8.0, h: 8.0, gw: 0.0, pos: Pos2::ZERO, layer: 0 });
        for b in bots {
            edges.push((b, j, net.clone()));
        }
        for t in tops {
            edges.push((j, t, net.clone()));
        }
    }
    let n0 = nodes.len();
    // cycle breaking by DFS
    let mut out: Vec<Vec<usize>> = vec![vec![]; n0];
    for (k, e) in edges.iter().enumerate() {
        out[e.0].push(k);
    }
    let mut reversed = vec![false; edges.len()];
    let mut state = vec![0u8; n0];
    for s in 0..n0 {
        if state[s] != 0 {
            continue;
        }
        let mut stack: Vec<(usize, usize)> = vec![(s, 0)];
        state[s] = 1;
        while let Some(&mut (v, ref mut i)) = stack.last_mut() {
            if *i < out[v].len() {
                let k = out[v][*i];
                *i += 1;
                let w = edges[k].1;
                if w == v {
                    continue;
                }
                if state[w] == 1 {
                    reversed[k] = true;
                } else if state[w] == 0 {
                    state[w] = 1;
                    stack.push((w, 0));
                }
            } else {
                state[v] = 2;
                stack.pop();
            }
        }
    }
    let dag: Vec<(usize, usize)> = edges.iter().enumerate().filter(|(_, e)| e.0 != e.1).map(|(k, e)| if reversed[k] { (e.1, e.0) } else { (e.0, e.1) }).collect();
    // longest path layering
    let mut indeg = vec![0usize; n0];
    let mut succ: Vec<Vec<usize>> = vec![vec![]; n0];
    for &(a, b) in &dag {
        succ[a].push(b);
        indeg[b] += 1;
    }
    let mut layer = vec![0usize; n0];
    let mut q: Vec<usize> = (0..n0).filter(|&v| indeg[v] == 0).collect();
    let mut qi = 0;
    while qi < q.len() {
        let v = q[qi];
        qi += 1;
        for &w in &succ[v] {
            layer[w] = layer[w].max(layer[v] + 1);
            indeg[w] -= 1;
            if indeg[w] == 0 {
                q.push(w);
            }
        }
    }
    // pull sources down next to their first successor (shorter wires)
    for &v in q.iter().rev() {
        if !succ[v].is_empty() {
            let m = succ[v].iter().map(|&w| layer[w]).min().unwrap();
            if m > layer[v] + 1 {
                layer[v] = m - 1;
            }
        }
    }
    for (v, l) in layer.iter().enumerate() {
        nodes[v].layer = *l;
    }
    // chains with dummies: (net, original edge index, node list top to bottom)
    let mut chains: Vec<(String, usize, Vec<usize>)> = vec![];
    for (k, e) in edges.iter().enumerate() {
        if e.0 == e.1 {
            continue;
        }
        let (a, b) = if reversed[k] { (e.1, e.0) } else { (e.0, e.1) };
        let mut chain = vec![a];
        for l in layer[a] + 1..layer[b] {
            let d = nodes.len();
            nodes.push(XNode { kind: XKind::Dummy, w: 0.0, h: 0.0, gw: 0.0, pos: Pos2::ZERO, layer: l });
            chain.push(d);
        }
        chain.push(b);
        chains.push((e.2.clone(), k, chain));
    }
    let nn = nodes.len();
    let nl = nodes.iter().map(|n| n.layer).max().unwrap_or(0) + 1;
    let mut layers: Vec<Vec<usize>> = vec![vec![]; nl];
    for (v, n) in nodes.iter().enumerate() {
        layers[n.layer].push(v);
    }
    let mut up: Vec<Vec<usize>> = vec![vec![]; nn];
    let mut dn: Vec<Vec<usize>> = vec![vec![]; nn];
    for (_, _, c) in &chains {
        for w in c.windows(2) {
            dn[w[0]].push(w[1]);
            up[w[1]].push(w[0]);
        }
    }
    // x of the port given an order: cumulative widths
    let place_by_order = |nodes: &mut Vec<XNode>, layers: &Vec<Vec<usize>>| {
        for l in layers {
            let mut x = 0.0;
            for (i, &v) in l.iter().enumerate() {
                if i > 0 {
                    x += sep(&nodes[l[i - 1]], &nodes[v]);
                }
                nodes[v].pos.x = x;
                x += nodes[v].w;
            }
        }
    };
    place_by_order(&mut nodes, &layers);
    for sweep in 0..12 {
        let down = sweep % 2 == 0;
        let ls: Vec<usize> = if down { (1..nl).collect() } else { (0..nl.saturating_sub(1)).rev().collect() };
        for l in ls {
            let mut keyed: Vec<(f32, usize, usize)> = layers[l]
                .iter()
                .enumerate()
                .map(|(i, &v)| {
                    let nb = if down { &up[v] } else { &dn[v] };
                    let own = nodes[v].pos.x + nodes[v].port_dx();
                    let k = if nb.is_empty() { own } else { nb.iter().map(|&u| nodes[u].pos.x + nodes[u].port_dx()).sum::<f32>() / nb.len() as f32 };
                    (k, i, v)
                })
                .collect();
            keyed.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap().then(a.1.cmp(&b.1)));
            layers[l] = keyed.into_iter().map(|t| t.2).collect();
            place_by_order(&mut nodes, &layers);
        }
    }
    // priority placement: port x towards the mean port x of neighbours, order and spacing kept
    for sweep in 0..8 {
        let down = sweep % 2 == 0;
        let ls: Vec<usize> = if down { (1..nl).collect() } else { (0..nl.saturating_sub(1)).rev().collect() };
        for l in ls {
            let lv = layers[l].clone();
            let n = lv.len();
            let mut left: Vec<f32> = lv.iter().map(|&v| nodes[v].pos.x).collect();
            let ws: Vec<f32> = lv.iter().map(|&v| nodes[v].w).collect();
            let gaps: Vec<f32> = (0..n.saturating_sub(1)).map(|i| sep(&nodes[lv[i]], &nodes[lv[i + 1]])).collect();
            let mut want = vec![f32::NAN; n];
            let mut prio = vec![0i64; n];
            for (i, &v) in lv.iter().enumerate() {
                let nb = if down { &up[v] } else { &dn[v] };
                if nb.is_empty() {
                    continue;
                }
                let m = nb.iter().map(|&u| nodes[u].pos.x + nodes[u].port_dx()).sum::<f32>() / nb.len() as f32;
                want[i] = m - nodes[v].port_dx();
                prio[i] = if matches!(nodes[v].kind, XKind::Dummy) { 1_000_000 } else { nb.len() as i64 };
            }
            let mut idx: Vec<usize> = (0..n).filter(|&i| !want[i].is_nan()).collect();
            idx.sort_by(|&a, &b| prio[b].cmp(&prio[a]));
            let mut fixed = vec![false; n];
            for i in idx {
                let mut t = want[i];
                let mut acc = 0.0;
                for j in (0..i).rev() {
                    acc += ws[j] + gaps[j];
                    if fixed[j] {
                        t = t.max(left[j] + acc);
                        break;
                    }
                }
                acc = 0.0;
                for j in i + 1..n {
                    acc += ws[j - 1] + gaps[j - 1];
                    if fixed[j] {
                        t = t.min(left[j] - acc);
                        break;
                    }
                }
                left[i] = t;
                fixed[i] = true;
                for j in (0..i).rev() {
                    let lim = left[j + 1] - ws[j] - gaps[j];
                    if left[j] > lim { left[j] = lim } else { break }
                }
                for j in i + 1..n {
                    let lim = left[j - 1] + ws[j - 1] + gaps[j - 1];
                    if left[j] < lim { left[j] = lim } else { break }
                }
            }
            for (i, &v) in lv.iter().enumerate() {
                nodes[v].pos.x = left[i];
            }
        }
    }
    // channels: one track per (net, upper port) between layers
    let mut layer_h = vec![0f32; nl];
    for n in &nodes {
        layer_h[n.layer] = layer_h[n.layer].max(n.h);
    }
    // segments in channel l (between layer l and l+1): (net, x_top, x_bottom, chain index)
    let mut chan: Vec<Vec<(String, f32, f32)>> = vec![vec![]; nl];
    for (net, _, c) in &chains {
        for w in c.windows(2) {
            let a = &nodes[w[0]];
            let b = &nodes[w[1]];
            chan[a.layer].push((net.clone(), a.pos.x + a.port_dx(), b.pos.x + b.port_dx()));
        }
    }
    // track per (net, top x); key -> index
    let mut tracks: Vec<HashMap<(String, i64), usize>> = vec![HashMap::new(); nl];
    let mut ntracks = vec![0usize; nl];
    for l in 0..nl {
        let mut groups: Vec<((String, i64), f32, f32)> = vec![];
        for (net, xt, xb) in &chan[l] {
            if (xt - xb).abs() < 0.5 {
                continue;
            }
            let key = (net.clone(), (*xt * 2.0).round() as i64);
            match groups.iter_mut().find(|g| g.0 == key) {
                Some(g) => {
                    g.1 = g.1.min(*xb);
                    g.2 = g.2.max(*xb);
                }
                None => groups.push((key, xt.min(*xb), xt.max(*xb))),
            }
        }
        for g in groups.iter_mut() {
            let xt = g.0 .1 as f32 / 2.0;
            g.1 = g.1.min(xt);
            g.2 = g.2.max(xt);
        }
        // left-edge packing: groups whose spans do not overlap share a track row
        groups.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap());
        let mut row_end: Vec<f32> = vec![];
        for g in &groups {
            let r = match row_end.iter().position(|&e| e + 6.0 < g.1) {
                Some(r) => r,
                None => {
                    row_end.push(f32::NEG_INFINITY);
                    row_end.len() - 1
                }
            };
            row_end[r] = g.2;
            tracks[l].insert(g.0.clone(), r);
        }
        ntracks[l] = row_end.len();
    }
    let inputs: Vec<String> = {
        let sd: HashSet<&String> = sc.mos.iter().flat_map(|d| [&d.d, &d.s]).collect();
        sc.ports.iter().filter(|p| !sd.contains(p) && !is_sup(p)).cloned().collect()
    };
    let legend_w = if inputs.is_empty() { 0.0 } else { 36.0 + inputs.iter().map(|s| text_w(s)).fold(0.0, f32::max) };
    let min_x = nodes.iter().map(|n| n.pos.x).fold(f32::INFINITY, f32::min);
    let dx = 20.0 + legend_w - if min_x.is_finite() { min_x } else { 0.0 };
    // reversed edges get their own tracks: one in the channel under the real bottom terminal, one in the
    // channel above the real top terminal (or above the first layer)
    let mut extra = vec![0usize; nl];
    let mut top_extra = 0usize;
    let mut back_tracks: HashMap<usize, (usize, Option<usize>)> = HashMap::new(); // chain -> (low idx, high idx or top idx)
    for (ci, (_, k, c)) in chains.iter().enumerate() {
        if !reversed[*k] {
            continue;
        }
        let la = nodes[*c.last().unwrap()].layer;
        let lb = nodes[c[0]].layer;
        let lo = extra[la];
        extra[la] += 1;
        let hi = if lb == 0 {
            top_extra += 1;
            None
        } else {
            extra[lb - 1] += 1;
            Some(extra[lb - 1] - 1)
        };
        back_tracks.insert(ci, (lo, hi.or(Some(usize::MAX - (top_extra - 1)))));
    }
    let mut ly = vec![0f32; nl];
    let mut chan_y = vec![0f32; nl];
    let mut y = 34.0 + top_extra as f32 * TRACK;
    for l in 0..nl {
        ly[l] = y;
        y += layer_h[l];
        // tracks stay clear of the VSS symbols below this layer (14 px) and the VDD symbols and labels
        // above the next one (about 22 px)
        let used = ntracks[l] + extra[l];
        let gap = LAYER_GAP.max(40.0 + used.saturating_sub(1) as f32 * TRACK);
        chan_y[l] = y + 18.0;
        y += gap;
    }
    for n in nodes.iter_mut() {
        n.pos.x += dx;
        n.pos.y = ly[n.layer] + (layer_h[n.layer] - n.h) / 2.0;
    }
    // wires
    let mut wires = vec![];
    let right = nodes.iter().map(|n| n.pos.x + n.w).fold(0.0, f32::max);
    let mut back = 0;
    for (ci, (net, k, c)) in chains.iter().enumerate() {
        let mut pts: Vec<Pos2> = vec![];
        for w in c.windows(2) {
            let a = &nodes[w[0]];
            let b = &nodes[w[1]];
            let p = a.bot_port();
            let q = b.top_port();
            if pts.is_empty() {
                pts.push(p);
            } else if (pts.last().unwrap().x - p.x).abs() > 0.1 || (pts.last().unwrap().y - p.y).abs() > 0.1 {
                pts.push(p);
            }
            if (p.x - q.x).abs() >= 0.5 {
                let key = (net.clone(), ((a.pos.x - dx + a.port_dx()) * 2.0).round() as i64);
                let t = tracks[a.layer].get(&key).copied().unwrap_or(0);
                let ty = chan_y[a.layer] + t as f32 * TRACK;
                pts.push(Pos2::new(p.x, ty));
                pts.push(Pos2::new(q.x, ty));
            }
            pts.push(q);
        }
        if reversed[*k] {
            // drawn from the real bottom terminal (lower node) around the right side to the real top terminal
            let a = &nodes[*c.last().unwrap()];
            let b = &nodes[c[0]];
            let p = a.bot_port();
            let q = b.top_port();
            back += 1;
            let xs = right + 14.0 + 8.0 * back as f32;
            let (lo, hi) = back_tracks[&ci];
            let y_lo = chan_y[a.layer] + (ntracks[a.layer] + lo) as f32 * TRACK;
            let y_hi = match hi {
                Some(h) if h <= usize::MAX / 2 => chan_y[b.layer - 1] + (ntracks[b.layer - 1] + h) as f32 * TRACK,
                Some(h) => 10.0 + (usize::MAX - h) as f32 * TRACK,
                None => q.y - 10.0,
            };
            pts = vec![p, Pos2::new(p.x, y_lo), Pos2::new(xs, y_lo), Pos2::new(xs, y_hi), Pos2::new(q.x, y_hi), q];
        }
        wires.push(XWire { net: net.clone(), pts });
    }
    let w = nodes.iter().map(|n| n.pos.x + n.w + 60.0).fold(right + 20.0 + 8.0 * back as f32, f32::max) + 40.0;
    let h = y + 10.0;
    let h = h.max(34.0 + 16.0 * inputs.len() as f32 + 20.0);
    XLayout { nodes, wires, orient: or, sup, inputs, size: Vec2::new(w, h) }
}

fn sep(a: &XNode, b: &XNode) -> f32 {
    if matches!(a.kind, XKind::Dummy) || matches!(b.kind, XKind::Dummy) {
        DUMMY_SEP
    } else {
        NODE_SEP
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum XHit {
    Dev(String),
    Net(String),
}

/// draw; `sel` is the selected device or net
pub fn paint(cv: &mut dyn Canvas, rect: Rect, view: &View, lay: &XLayout, sc: &Subckt, sel: Option<&XHit>, col: &Colors) {
    let z = view.zoom;
    let tp = |p: Pos2| view.to_screen(rect, p);
    let sel_net = match sel {
        Some(XHit::Net(n)) => Some(n.as_str()),
        _ => None,
    };
    let sel_dev = match sel {
        Some(XHit::Dev(n)) => Some(n.as_str()),
        _ => None,
    };
    let wire_st = |net: &str| if Some(net) == sel_net { Stroke::new((2.6 * z).max(1.6), col.hl) } else { Stroke::new((1.2 * z).max(0.6), col.wire) };
    let ink_st = Stroke::new((1.3 * z).max(0.7), col.ink);
    let fs = 11.0 * z;
    let show_text = fs >= 4.0;
    for w in &lay.wires {
        cv.line(w.pts.iter().map(|&p| tp(p)).collect(), wire_st(&w.net));
    }
    for n in &lay.nodes {
        match &n.kind {
            XKind::Junction { net, port } => {
                let c = tp(n.pos + Vec2::new(4.0, 4.0));
                let colr = if Some(net.as_str()) == sel_net { col.hl } else { col.wire };
                cv.circle(c, (if *port { 3.5 } else { 2.6 }) * z, Some(colr), Stroke::NONE);
                if show_text {
                    if *port {
                        cv.text(tp(n.pos + Vec2::new(12.0, 4.0)), egui::Align2::LEFT_CENTER, net, fs, if Some(net.as_str()) == sel_net { col.hl } else { col.ink });
                    } else {
                        cv.text(tp(n.pos + Vec2::new(9.0, -2.0)), egui::Align2::LEFT_BOTTOM, net, fs * 0.85, if Some(net.as_str()) == sel_net { col.hl } else { col.muted });
                    }
                }
            }
            XKind::Dev(i) => draw_dev(cv, &tp, n, &sc.mos[*i], &lay.orient[*i], &lay.sup, z, sel_net, sel_dev, col, show_text),
            XKind::Dummy => {}
        }
    }
    for (i, p) in lay.inputs.iter().enumerate() {
        let y = 16.0 + 16.0 * i as f32;
        let c = if Some(p.as_str()) == sel_net { col.hl } else { col.ink };
        cv.closed(vec![tp(Pos2::new(8.0, y)), tp(Pos2::new(20.0, y)), tp(Pos2::new(24.0, y - 4.0)), tp(Pos2::new(8.0, y - 4.0))].into_iter().rev().collect(), None, Stroke::new(ink_st.width, c));
        if show_text {
            cv.text(tp(Pos2::new(28.0, y - 2.0)), egui::Align2::LEFT_CENTER, p, fs, c);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_dev(cv: &mut dyn Canvas, tp: &dyn Fn(Pos2) -> Pos2, n: &XNode, m: &Mos, o: &(String, String), sup: &Supplies, z: f32, sel_net: Option<&str>, sel_dev: Option<&str>, col: &Colors, show_text: bool) {
    let x = n.pos.x + n.gw;
    let y0 = n.pos.y;
    let p = |dx: f32, dy: f32| tp(Pos2::new(x + dx, y0 + dy));
    let selected = sel_dev == Some(m.name.as_str());
    let body = if selected { col.sel } else { col.ink };
    let st = Stroke::new((1.3 * z).max(0.7), body);
    let net_st = |net: &str| if Some(net) == sel_net { Stroke::new((2.6 * z).max(1.6), col.hl) } else { st };
    let pm = m.typ == "pmos";
    let h2 = DH / 2.0;
    if selected {
        cv.closed(vec![p(-2.0, -2.0), p(DW + 2.0, -2.0), p(DW + 2.0, DH + 2.0), p(-2.0, DH + 2.0)], Some(col.fill), Stroke::new((1.0 * z).max(0.6), col.sel));
    }
    let fs = 11.0 * z;
    if show_text {
        cv.text(p(-3.0, h2), egui::Align2::RIGHT_CENTER, &m.g, fs, if Some(m.g.as_str()) == sel_net { col.hl } else { col.ink });
    }
    let gx = if pm { 4.0 } else { 10.0 };
    cv.line(vec![p(0.0, h2), p(gx, h2)], net_st(&m.g));
    if pm {
        cv.circle(p(7.0, h2), 3.0 * z, None, net_st(&m.g));
    }
    cv.line(vec![p(12.0, h2 - 13.0), p(12.0, h2 + 13.0)], net_st(&m.g));
    cv.line(vec![p(17.0, h2 - 15.0), p(17.0, h2 + 15.0)], Stroke::new(st.width * 1.6, body));
    cv.line(vec![p(17.0, h2 - 12.0), p(28.0, h2 - 12.0), p(28.0, 0.0)], net_st(&o.0));
    cv.line(vec![p(17.0, h2 + 12.0), p(28.0, h2 + 12.0), p(28.0, DH)], net_st(&o.1));
    let src_top = m.s == o.0;
    let ay = if src_top { h2 - 12.0 } else { h2 + 12.0 };
    if pm {
        cv.closed(vec![p(19.0, ay), p(24.0, ay - 2.5), p(24.0, ay + 2.5)], Some(body), Stroke::NONE);
    } else {
        cv.closed(vec![p(26.0, ay), p(21.0, ay - 2.5), p(21.0, ay + 2.5)], Some(body), Stroke::NONE);
    }
    if show_text {
        let fins = if m.nfin.is_finite() { format!("{}f", m.nfin) } else if m.w.is_finite() { format!("{}n", (m.w * 1e10).round() / 10.0) } else { String::new() };
        cv.text(p(31.0, h2 - 2.0), egui::Align2::LEFT_BOTTOM, &m.name, fs * 0.85, if selected { col.sel } else { col.muted });
        cv.text(p(31.0, h2 + 9.0), egui::Align2::LEFT_BOTTOM, &fins, fs * 0.8, col.muted);
    }
    for (net, yy, top) in [(&o.0, 0.0, true), (&o.1, DH, false)] {
        let ss = net_st(net);
        let tc = if Some(net.as_str()) == sel_net { col.hl } else { col.ink };
        if Some(net.as_str()) == sup.vdd.as_deref() && top {
            cv.line(vec![p(28.0, yy), p(28.0, yy - 8.0)], ss);
            cv.line(vec![p(21.0, yy - 8.0), p(35.0, yy - 8.0)], ss);
            if show_text {
                cv.text(p(28.0, yy - 10.0), egui::Align2::CENTER_BOTTOM, net, fs * 0.85, tc);
            }
        } else if Some(net.as_str()) == sup.vss.as_deref() {
            cv.line(vec![p(28.0, yy), p(28.0, yy + 8.0)], ss);
            cv.line(vec![p(21.0, yy + 8.0), p(35.0, yy + 8.0)], ss);
            cv.line(vec![p(24.0, yy + 11.0), p(32.0, yy + 11.0)], ss);
            cv.line(vec![p(26.5, yy + 14.0), p(29.5, yy + 14.0)], ss);
            if show_text {
                cv.text(p(40.0, yy + 12.0), egui::Align2::LEFT_CENTER, net, fs * 0.85, tc);
            }
        } else if Some(net.as_str()) == sup.vdd.as_deref() {
            cv.line(vec![p(28.0, yy), p(28.0, yy + 8.0)], ss);
            cv.line(vec![p(21.0, yy + 8.0), p(35.0, yy + 8.0)], ss);
            if show_text {
                cv.text(p(40.0, yy + 9.0), egui::Align2::LEFT_CENTER, net, fs * 0.85, tc);
            }
        }
    }
}

fn seg_dist(p: Pos2, a: Pos2, b: Pos2) -> f32 {
    let ab = b - a;
    let l2 = ab.length_sq();
    let t = if l2 == 0.0 { 0.0 } else { ((p - a).dot(ab) / l2).clamp(0.0, 1.0) };
    (a + ab * t - p).length()
}

/// hit test in layout coordinates; tol in layout units
pub fn hit_test(lay: &XLayout, sc: &Subckt, p: Pos2, tol: f32) -> Option<XHit> {
    for n in &lay.nodes {
        if let XKind::Dev(i) = n.kind {
            let m = &sc.mos[i];
            let x = n.pos.x + n.gw;
            // gate label
            let lw = text_w(&m.g);
            if Rect::from_min_max(Pos2::new(x - 3.0 - lw, n.pos.y + DH / 2.0 - 8.0), Pos2::new(x + 12.0, n.pos.y + DH / 2.0 + 8.0)).contains(p) {
                return Some(XHit::Net(m.g.clone()));
            }
            // supply symbols
            let o = &lay.orient[i];
            for (net, yy, dir) in [(&o.0, n.pos.y, -1.0f32), (&o.1, n.pos.y + DH, 1.0)] {
                if Some(net.as_str()) == lay.sup.vdd.as_deref() || Some(net.as_str()) == lay.sup.vss.as_deref() {
                    let r = Rect::from_two_pos(Pos2::new(x + 18.0, yy), Pos2::new(x + 40.0, yy + dir * 18.0));
                    if r.expand(tol).contains(p) {
                        return Some(XHit::Net(net.clone()));
                    }
                }
            }
            if Rect::from_min_size(Pos2::new(x + 14.0, n.pos.y + 4.0), Vec2::new(DW - 14.0, DH - 8.0)).contains(p) {
                return Some(XHit::Dev(m.name.clone()));
            }
            if Rect::from_min_size(Pos2::new(x, n.pos.y), Vec2::new(DW, DH)).contains(p) {
                return Some(XHit::Dev(m.name.clone()));
            }
        }
        if let XKind::Junction { net, .. } = &n.kind {
            if (n.pos + Vec2::new(4.0, 4.0) - p).length() <= 5.0 + tol {
                return Some(XHit::Net(net.clone()));
            }
        }
    }
    for w in &lay.wires {
        for s in w.pts.windows(2) {
            if seg_dist(p, s[0], s[1]) <= tol {
                return Some(XHit::Net(w.net.clone()));
            }
        }
    }
    for (i, inp) in lay.inputs.iter().enumerate() {
        let y = 16.0 + 16.0 * i as f32;
        if Rect::from_min_max(Pos2::new(6.0, y - 10.0), Pos2::new(28.0 + text_w(inp), y + 4.0)).contains(p) {
            return Some(XHit::Net(inp.clone()));
        }
    }
    None
}

/// centre of a device or of a net's first element, for scrolling it into view
pub fn locate(lay: &XLayout, sc: &Subckt, h: &XHit) -> Option<Pos2> {
    match h {
        XHit::Dev(name) => lay.nodes.iter().find(|n| matches!(n.kind, XKind::Dev(i) if sc.mos[i].name == *name)).map(|n| n.pos + Vec2::new(n.w / 2.0, n.h / 2.0)),
        XHit::Net(net) => lay
            .wires
            .iter()
            .find(|w| w.net == *net)
            .and_then(|w| w.pts.first().copied())
            .or_else(|| lay.nodes.iter().find(|n| matches!(&n.kind, XKind::Junction { net: j, .. } if j == net)).map(|n| n.pos))
            .or_else(|| lay.nodes.iter().find(|n| matches!(n.kind, XKind::Dev(i) if sc.mos[i].g == *net)).map(|n| n.pos + Vec2::new(n.gw / 2.0, DH / 2.0))),
    }
}

pub fn to_svg(lay: &XLayout, sc: &Subckt, sel: Option<&XHit>, col: &Colors) -> String {
    let mut svg = crate::schematic::Svg { body: String::new() };
    let rect = Rect::from_min_size(Pos2::ZERO, lay.size);
    let view = View { pan: Vec2::ZERO, zoom: 1.0 };
    paint(&mut svg, rect, &view, lay, sc, sel, col);
    format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{:.0}" height="{:.0}" viewBox="0 0 {:.0} {:.0}"><rect width="100%" height="100%" fill="#ffffff"/>{}</svg>"##, lay.size.x, lay.size.y, lay.size.x, lay.size.y, svg.body)
}

/// count of crossings between wire segments of different nets (layout quality check)
pub fn crossings(lay: &XLayout) -> usize {
    let mut segs: Vec<(&str, Pos2, Pos2)> = vec![];
    for w in &lay.wires {
        for s in w.pts.windows(2) {
            segs.push((&w.net, s[0], s[1]));
        }
    }
    let mut c = 0;
    for i in 0..segs.len() {
        for j in i + 1..segs.len() {
            let (na, a1, a2) = segs[i];
            let (nb, b1, b2) = segs[j];
            if na == nb {
                continue;
            }
            let av = (a1.x - a2.x).abs() < 0.01;
            let bv = (b1.x - b2.x).abs() < 0.01;
            if av == bv {
                continue;
            }
            let (v1, v2, h1, h2) = if av { (a1, a2, b1, b2) } else { (b1, b2, a1, a2) };
            let x = v1.x;
            let y = h1.y;
            if x > h1.x.min(h2.x) + 0.01 && x < h1.x.max(h2.x) - 0.01 && y > v1.y.min(v2.y) + 0.01 && y < v1.y.max(v2.y) - 0.01 {
                c += 1;
            }
        }
    }
    c
}
