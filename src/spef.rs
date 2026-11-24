//! SPEF reader (IEEE 1481) for block-level parasitics, such as the output of OpenROAD's OpenRCX, and the metal
//! segments of a block layout that its resistors and capacitors sit on. With node coordinates (`*C x y` on `*CONN`
//! pins and `*N` internal nodes, as `write_spef -coordinates` writes them) every R is placed between its two node
//! points on the routing of its net in the DEF.
use crate::def::Layout;
use crate::lpe::{corridor, SegKind};
use std::collections::HashMap;

/// one node of a net: a port, an instance pin or an internal node
#[derive(Clone, Debug)]
pub struct SpefNode {
    /// name with the name map applied: `net:3`, `inst:pin` or a port name
    pub name: String,
    /// location in µm, when the SPEF gives one
    pub xy: Option<[f64; 2]>,
}

/// a capacitor of a net: to ground, or a coupling capacitor to a node of another net
#[derive(Clone, Debug)]
pub struct SpefCap {
    pub name: String,
    /// node of this net
    pub a: usize,
    /// for a coupling capacitor: the other node's name, and (net, node) once resolved
    pub other: Option<String>,
    pub other_at: Option<(usize, usize)>,
    pub farads: f64,
}

#[derive(Clone, Debug)]
pub struct SpefRes {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub ohms: f64,
}

#[derive(Clone, Debug, Default)]
pub struct SpefNet {
    pub name: String,
    /// total capacitance in farads (the `*D_NET` value)
    pub total_f: f64,
    pub nodes: Vec<SpefNode>,
    pub node_index: HashMap<String, usize>,
    pub res: Vec<SpefRes>,
    pub caps: Vec<SpefCap>,
}

#[derive(Clone, Debug, Default)]
pub struct Spef {
    pub design: String,
    pub nets: Vec<SpefNet>,
    pub by_name: HashMap<String, usize>,
    /// nodes with a location, and all nodes
    pub located: usize,
    pub node_count: usize,
    pub warnings: Vec<String>,
}

fn unit(tokens: &[&str], base: &[(&str, f64)]) -> f64 {
    let k = tokens.get(1).and_then(|v| v.parse::<f64>().ok()).unwrap_or(1.0);
    let u = tokens.get(2).map(|u| u.to_ascii_uppercase()).unwrap_or_default();
    base.iter().find(|(n, _)| *n == u).map(|(_, f)| k * f).unwrap_or(k)
}

/// Read a SPEF text. Names keep the hierarchy divider of the file; `*NAME_MAP` references are replaced by names.
pub fn parse_spef(text: &str) -> Result<Spef, String> {
    let mut s = Spef::default();
    let (mut cu, mut ru) = (1e-12, 1.0);
    let mut map: HashMap<String, String> = HashMap::new();
    let mut delim = ':';
    #[derive(PartialEq)]
    enum Sec {
        None,
        Map,
        Conn,
        Cap,
        Res,
        Other,
    }
    let mut sec = Sec::None;
    let mut cur: Option<SpefNet> = None;
    // a name with its name-map reference replaced: `*12` or `*12:A` / `*12:5`
    // SPEF escapes special characters of names with a backslash (`u0\.r0\.rcnt[0]`); the DEF spells them plainly
    let unescape = |s: &str| -> String {
        let mut o = String::with_capacity(s.len());
        let mut esc = false;
        for c in s.chars() {
            if c == '\\' && !esc {
                esc = true;
                continue;
            }
            esc = false;
            o.push(c);
        }
        o
    };
    let resolve = |map: &HashMap<String, String>, delim: char, t: &str| -> String {
        if let Some(r) = t.strip_prefix('*') {
            let (head, tail) = match r.split_once(delim) {
                Some((h, t)) => (h, Some(t)),
                None => (r, None),
            };
            if let Some(n) = map.get(head) {
                return unescape(&match tail {
                    Some(t) => format!("{}{}{}", n, delim, t),
                    None => n.clone(),
                });
            }
        }
        unescape(t)
    };
    let node_of = |net: &mut SpefNet, name: String| -> usize {
        if let Some(&i) = net.node_index.get(&name) {
            return i;
        }
        net.nodes.push(SpefNode { name: name.clone(), xy: None });
        net.node_index.insert(name, net.nodes.len() - 1);
        net.nodes.len() - 1
    };
    for (ln, raw) in text.lines().enumerate() {
        let line = raw.split("//").next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let t: Vec<&str> = line.split_whitespace().collect();
        let key = t[0];
        if key.starts_with('*') && !key[1..].starts_with(|c: char| c.is_ascii_digit()) {
            match key {
                "*DESIGN" => s.design = t.get(1).map(|d| d.trim_matches('"').to_string()).unwrap_or_default(),
                "*DELIMITER" => delim = t.get(1).and_then(|d| d.chars().next()).unwrap_or(':'),
                "*C_UNIT" => cu = unit(&t, &[("PF", 1e-12), ("FF", 1e-15), ("NF", 1e-9), ("UF", 1e-6), ("F", 1.0)]),
                "*R_UNIT" => ru = unit(&t, &[("OHM", 1.0), ("KOHM", 1e3), ("MOHM", 1e6)]),
                "*NAME_MAP" => sec = Sec::Map,
                "*D_NET" => {
                    let name = resolve(&map, delim, t.get(1).copied().unwrap_or(""));
                    let total = t.get(2).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) * cu;
                    cur = Some(SpefNet { name, total_f: total, ..Default::default() });
                    sec = Sec::Other;
                }
                "*CONN" => sec = Sec::Conn,
                "*CAP" => sec = Sec::Cap,
                "*RES" => sec = Sec::Res,
                "*END" => {
                    if let Some(n) = cur.take() {
                        s.by_name.insert(n.name.clone(), s.nets.len());
                        s.nets.push(n);
                    }
                    sec = Sec::None;
                }
                "*P" | "*I" | "*N" if sec == Sec::Conn => {
                    let Some(net) = cur.as_mut() else { continue };
                    let name = resolve(&map, delim, t.get(1).copied().unwrap_or(""));
                    let i = node_of(net, name);
                    if let Some(k) = t.iter().position(|x| *x == "*C") {
                        if let (Some(x), Some(y)) = (t.get(k + 1).and_then(|v| v.parse::<f64>().ok()), t.get(k + 2).and_then(|v| v.parse::<f64>().ok())) {
                            net.nodes[i].xy = Some([x, y]);
                        }
                    }
                }
                _ => {
                    if sec == Sec::Map || sec == Sec::None {
                        sec = Sec::Other;
                    }
                }
            }
            continue;
        }
        match sec {
            Sec::Map if t.len() >= 2 => {
                map.insert(t[0].trim_start_matches('*').to_string(), t[1].to_string());
            }
            Sec::Cap => {
                let Some(net) = cur.as_mut() else { continue };
                if t.len() == 3 {
                    let a = node_of(net, resolve(&map, delim, t[1]));
                    let f = t[2].parse::<f64>().map_err(|_| format!("line {}: bad capacitance {}", ln + 1, t[2]))? * cu;
                    net.caps.push(SpefCap { name: t[0].to_string(), a, other: None, other_at: None, farads: f });
                } else if t.len() >= 4 {
                    let (na, nb) = (resolve(&map, delim, t[1]), resolve(&map, delim, t[2]));
                    let f = t[3].parse::<f64>().map_err(|_| format!("line {}: bad capacitance {}", ln + 1, t[3]))? * cu;
                    // the node of this net is the one named after the net or listed in its connections
                    let mine_b = !net.node_index.contains_key(&na) && (net.node_index.contains_key(&nb) || nb.starts_with(&format!("{}{}", net.name, delim)));
                    let (m, o) = if mine_b { (nb, na) } else { (na, nb) };
                    let a = node_of(net, m);
                    net.caps.push(SpefCap { name: t[0].to_string(), a, other: Some(o), other_at: None, farads: f });
                }
            }
            Sec::Res if t.len() >= 4 => {
                let Some(net) = cur.as_mut() else { continue };
                let a = node_of(net, resolve(&map, delim, t[1]));
                let b = node_of(net, resolve(&map, delim, t[2]));
                let r = t[3].parse::<f64>().map_err(|_| format!("line {}: bad resistance {}", ln + 1, t[3]))? * ru;
                net.res.push(SpefRes { name: t[0].to_string(), a, b, ohms: r });
            }
            _ => {}
        }
    }
    // the other side of each coupling capacitor: the net whose nodes hold that name
    let mut owner: HashMap<&str, (usize, usize)> = HashMap::new();
    for (ni, n) in s.nets.iter().enumerate() {
        for (k, nd) in n.nodes.iter().enumerate() {
            owner.entry(nd.name.as_str()).or_insert((ni, k));
        }
    }
    let found: Vec<Vec<Option<(usize, usize)>>> = s.nets.iter().map(|n| n.caps.iter().map(|c| c.other.as_deref().and_then(|o| owner.get(o).copied())).collect()).collect();
    for (n, f) in s.nets.iter_mut().zip(found) {
        for (c, o) in n.caps.iter_mut().zip(f) {
            c.other_at = o;
        }
    }
    s.node_count = s.nets.iter().map(|n| n.nodes.len()).sum();
    s.located = s.nets.iter().map(|n| n.nodes.iter().filter(|x| x.xy.is_some()).count()).sum();
    if s.nets.is_empty() {
        return Err("no *D_NET in the file".into());
    }
    Ok(s)
}

/// one metal segment of a block net: the routing between the two nodes of one R
#[derive(Clone, Debug)]
pub struct BlockSeg {
    pub kind: SegKind,
    /// layer name(s) of the segment
    pub layer: String,
    /// rectangles in DEF database units
    pub rects: Vec<[i32; 4]>,
    /// end points in DEF database units
    pub ends: [[f64; 2]; 2],
    pub res: Option<usize>,
    pub nodes: [usize; 2],
}

/// the segments of one SPEF net on the block layout
#[derive(Clone, Debug, Default)]
pub struct NetSegs {
    pub segs: Vec<BlockSeg>,
    /// segment of every R, or why it has none
    pub res: Vec<Result<usize, String>>,
    /// segment and point (database units) of every capacitor of the net on this net's side
    pub caps: Vec<Option<(usize, [f64; 2])>>,
}

/// Split one SPEF net into segments on the routing of its DEF net: an R whose two node points lie on one layer of
/// the net's wires is the wire between them (Wire); an R between two points of one place or on different layers is
/// the via there (Via); a capacitor goes on the wire segment under its node, else on a Patch of the shapes there.
pub fn net_segments(net: &SpefNet, lay: &Layout, lib: &crate::lef::Lib) -> NetSegs {
    let mut out = NetSegs::default();
    let Some(&li) = lay.net_index.get(&net.name) else {
        out.res = net.res.iter().map(|_| Err("the net is not in the DEF".to_string())).collect();
        out.caps = vec![None; net.caps.len()];
        return out;
    };
    // the routing of this net, by layer, and every shape with its layer and whether it belongs to a via
    let mut by_layer: HashMap<u8, Vec<[i32; 4]>> = HashMap::new();
    let mut shapes: Vec<(u8, [i32; 4], bool)> = vec![];
    for k in 0..lay.rects.len() {
        let kind = lay.rects.kind[k];
        if lay.rects.owner[k] == li as i32 && (kind == crate::K_WIRE || kind == crate::K_VIA || kind == crate::K_SWIRE || kind == crate::K_SVIA) {
            by_layer.entry(lay.rects.layer[k]).or_default().push(lay.rects.r[k]);
            shapes.push((lay.rects.layer[k], lay.rects.r[k], kind == crate::K_VIA || kind == crate::K_SVIA));
        }
    }
    // the pins the net ends on: block ports (DEF pins of this net) and cell pins (LEF macro pins of the instances)
    let mut add_pin = |l: u8, r: [i32; 4], shapes: &mut Vec<(u8, [i32; 4], bool)>| {
        by_layer.entry(l).or_default().push(r);
        shapes.push((l, r, false));
    };
    for (pid, p) in lay.pins.iter().enumerate() {
        if p.net.as_deref() == Some(net.name.as_str()) {
            for k in 0..lay.rects.len() {
                if lay.rects.kind[k] == crate::K_IOPIN && lay.rects.owner[k] == pid as i32 {
                    add_pin(lay.rects.layer[k], lay.rects.r[k], &mut shapes);
                }
            }
        }
    }
    for nd in &net.nodes {
        let Some((inst, pin)) = nd.name.rsplit_once(':') else { continue };
        let Some(c) = lay.comp_index.get(inst).and_then(|&ci| lay.comps.get(ci as usize)) else { continue };
        for (layer, pname, r) in crate::def::comp_shapes(lay, lib, c) {
            if pname.as_deref() == Some(pin) {
                if let Some(&l) = lay.layer_ids.get(&layer) {
                    add_pin(l, [r[0].round() as i32, r[1].round() as i32, r[2].round() as i32, r[3].round() as i32], &mut shapes);
                }
            }
        }
    }
    // shapes joined: touching on one layer, or touching in plan where one of them belongs to a via (built on first use)
    let mut adj: Option<Vec<Vec<usize>>> = None;
    let touch = |a: &[i32; 4], b: &[i32; 4]| a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3];
    let lname = |l: u8| lay.layer_names.get(l as usize).cloned().unwrap_or_else(|| format!("layer{}", l));
    let db = |p: [f64; 2]| [p[0] * lay.dbu, p[1] * lay.dbu];
    let tol = 0.5;
    let inside = |r: &[i32; 4], p: [f64; 2]| p[0] >= r[0] as f64 - tol && p[0] <= r[2] as f64 + tol && p[1] >= r[1] as f64 - tol && p[1] <= r[3] as f64 + tol;
    let mut layers: Vec<u8> = by_layer.keys().copied().collect();
    layers.sort();
    for (k, r) in net.res.iter().enumerate() {
        let (Some(pa), Some(pb)) = (net.nodes[r.a].xy.map(db), net.nodes[r.b].xy.map(db)) else {
            out.res.push(Err("an end node has no coordinate in the SPEF".into()));
            continue;
        };
        let same = (pa[0] - pb[0]).abs() <= tol && (pa[1] - pb[1]).abs() <= tol;
        // the share of the straight line from a to b that the layer's shapes cover (65 points along it)
        let cover = |q: &[[i32; 4]]| -> f64 {
            let n = 64;
            (0..=n).filter(|&s| {
                let t = s as f64 / n as f64;
                let p = [pa[0] + (pb[0] - pa[0]) * t, pa[1] + (pb[1] - pa[1]) * t];
                q.iter().any(|x| inside(x, p))
            }).count() as f64 / (n + 1) as f64
        };
        // the wire: the layer whose shapes cover the whole line between the two points; its shapes on that line
        let full: Option<u8> = if same { None } else { layers.iter().copied().find(|l| cover(&by_layer[l]) >= 0.999) };
        let (kind, rects, layer) = if let Some(l) = full {
            let bx = [pa[0].min(pb[0]) - tol, pa[1].min(pb[1]) - tol, pa[0].max(pb[0]) + tol, pa[1].max(pb[1]) + tol];
            let on_line: Vec<[i32; 4]> = by_layer[&l].iter().filter(|r| (r[0] as f64) <= bx[2] && (r[2] as f64) >= bx[0] && (r[1] as f64) <= bx[3] && (r[3] as f64) >= bx[1]).cloned().collect();
            (SegKind::Wire, corridor(&on_line, pa, pb), lname(l))
        } else if let Some(path) = (!same).then(|| {
            // a merged run that turns or changes layer: the shortest chain of joined shapes from a to b
            let adj = adj.get_or_insert_with(|| {
                let mut g = vec![vec![]; shapes.len()];
                for i in 0..shapes.len() {
                    for j in i + 1..shapes.len() {
                        if touch(&shapes[i].1, &shapes[j].1) && (shapes[i].0 == shapes[j].0 || shapes[i].2 || shapes[j].2) {
                            g[i].push(j);
                            g[j].push(i);
                        }
                    }
                }
                g
            });
            // the shapes at an end point; a cell pin point that no routing covers starts from the nearest shape
            let at_point = |p: [f64; 2]| -> Vec<usize> {
                let on: Vec<usize> = (0..shapes.len()).filter(|&i| inside(&shapes[i].1, p)).collect();
                if !on.is_empty() {
                    return on;
                }
                let d = |r: &[i32; 4]| {
                    let q = [p[0].clamp(r[0] as f64, r[2] as f64), p[1].clamp(r[1] as f64, r[3] as f64)];
                    (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)
                };
                (0..shapes.len()).min_by(|&a, &b| d(&shapes[a].1).partial_cmp(&d(&shapes[b].1)).unwrap_or(std::cmp::Ordering::Equal)).into_iter().collect()
            };
            let targets = at_point(pb);
            let mut prev = vec![usize::MAX; shapes.len()];
            let mut q = std::collections::VecDeque::new();
            for i in at_point(pa) {
                prev[i] = i;
                q.push_back(i);
            }
            let mut end = None;
            while let Some(i) = q.pop_front() {
                if targets.contains(&i) {
                    end = Some(i);
                    break;
                }
                for &j in &adj[i] {
                    if prev[j] == usize::MAX {
                        prev[j] = i;
                        q.push_back(j);
                    }
                }
            }
            let mut chain = vec![end?];
            while prev[*chain.last().unwrap()] != *chain.last().unwrap() {
                chain.push(prev[*chain.last().unwrap()]);
            }
            chain.reverse();
            Some(chain)
        }).flatten() {
            // the end shapes cut at the end points towards their neighbour on the chain, the middle ones whole
            let centre = |r: &[i32; 4]| [(r[0] + r[2]) as f64 / 2.0, (r[1] + r[3]) as f64 / 2.0];
            let mut rs = vec![];
            let mut names: Vec<String> = vec![];
            for (n, &i) in path.iter().enumerate() {
                let r = shapes[i].1;
                let cut = if path.len() == 1 {
                    corridor(&[r], pa, pb)
                } else if n == 0 {
                    corridor(&[r], pa, centre(&shapes[path[1]].1))
                } else if n + 1 == path.len() {
                    corridor(&[r], centre(&shapes[path[n - 1]].1), pb)
                } else {
                    vec![r]
                };
                rs.extend(if cut.is_empty() { vec![r] } else { cut });
                let nm = lname(shapes[i].0);
                if !names.contains(&nm) {
                    names.push(nm);
                }
            }
            (SegKind::Path, rs, names.join("+"))
        } else {
            // a via or a stack: the shapes of every layer at either point, cut to the box of the two points
            let mut rs = vec![];
            let mut names = vec![];
            for l in &layers {
                let at: Vec<[i32; 4]> = by_layer[l].iter().filter(|x| inside(x, pa) || inside(x, pb)).cloned().collect();
                if !at.is_empty() {
                    names.push(lname(*l));
                    rs.extend(corridor(&at, pa, pb));
                }
            }
            if rs.is_empty() {
                let h = 0.009 * lay.dbu;
                let m = [(pa[0] + pb[0]) / 2.0, (pa[1] + pb[1]) / 2.0];
                rs.push([(m[0] - h) as i32, (m[1] - h) as i32, (m[0] + h) as i32, (m[1] + h) as i32]);
            }
            (SegKind::Via, rs, if names.is_empty() { "?".into() } else { names.join("+") })
        };
        out.res.push(Ok(out.segs.len()));
        out.segs.push(BlockSeg { kind, layer, rects, ends: [pa, pb], res: Some(k), nodes: [r.a, r.b] });
    }
    // capacitors: the wire segment under the node point (one that ends at the node first), else a patch
    let mut patch: HashMap<usize, usize> = HashMap::new();
    for c in &net.caps {
        let Some(p) = net.nodes[c.a].xy.map(db) else {
            out.caps.push(None);
            continue;
        };
        let holds = |s: &BlockSeg| s.rects.iter().any(|r| inside(r, p));
        let pick = (0..out.segs.len()).filter(|&i| out.segs[i].kind == SegKind::Wire && holds(&out.segs[i])).min_by_key(|&i| !out.segs[i].nodes.contains(&c.a) as u8);
        let seg = match pick.or_else(|| (0..out.segs.len()).find(|&i| holds(&out.segs[i]))) {
            Some(i) => i,
            None => *patch.entry(c.a).or_insert_with(|| {
                let mut rs = vec![];
                let mut names = vec![];
                for l in &layers {
                    let at: Vec<[i32; 4]> = by_layer[l].iter().filter(|x| inside(x, p)).cloned().collect();
                    if !at.is_empty() {
                        names.push(lname(*l));
                        rs.extend(at);
                    }
                }
                if rs.is_empty() {
                    let h = 0.009 * lay.dbu;
                    rs.push([(p[0] - h) as i32, (p[1] - h) as i32, (p[0] + h) as i32, (p[1] + h) as i32]);
                }
                // no routing under the point: the pin of a cell (inst:pin) or a port of the block
                let what = if net.nodes[c.a].name.contains(':') { "pin" } else { "port" };
                out.segs.push(BlockSeg { kind: SegKind::Patch, layer: if names.is_empty() { what.into() } else { names.join("+") }, rects: rs, ends: [p, p], res: None, nodes: [c.a, c.a] });
                out.segs.len() - 1
            }),
        };
        out.caps.push(Some((seg, p)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::parse_spef;

    #[test]
    fn reads_names_units_coordinates_and_coupling() {
        let t = "*SPEF \"ieee 1481-1999\"\n*DESIGN \"top\"\n*DELIMITER :\n*C_UNIT 1 PF\n*R_UNIT 1 OHM\n\n*NAME_MAP\n*1 a\n*2 b\n*3 u1\n\n*D_NET *1 0.002\n*CONN\n*P a I *C 0.1 0.2\n*I *3:A I *C 1.0 0.2 *D INV\n*N *1:5 *C 0.5 0.2\n*CAP\n1 *1:5 0.001\n2 *1:5 *2:7 0.0005\n*RES\n1 a *1:5 10\n2 *1:5 *3:A 12.5\n*END\n\n*D_NET *2 0.001\n*CONN\n*N *2:7 *C 0.5 0.4\n*CAP\n1 *2:7 *1:5 0.0005\n*RES\n*END\n";
        let s = parse_spef(t).unwrap();
        assert_eq!(s.design, "top");
        assert_eq!(s.nets.len(), 2);
        let a = &s.nets[s.by_name["a"]];
        assert_eq!(a.nodes.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), vec!["a", "u1:A", "a:5"]);
        assert_eq!(a.nodes[2].xy, Some([0.5, 0.2]));
        assert_eq!(a.res.len(), 2);
        assert!((a.res[1].ohms - 12.5).abs() < 1e-12);
        assert!((a.caps[0].farads - 1e-15).abs() < 1e-24);
        assert_eq!(a.caps[1].other.as_deref(), Some("b:7"));
        assert_eq!(a.caps[1].other_at, Some((s.by_name["b"], 0)));
        // the coupling capacitor seen from b: its own node is b:7 although a:5 comes first on the line
        let b = &s.nets[s.by_name["b"]];
        assert_eq!(b.nodes[b.caps[0].a].name, "b:7");
        assert_eq!(s.located, 4);
    }
}
