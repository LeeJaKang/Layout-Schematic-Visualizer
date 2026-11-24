//! Hierarchy folding for the netlist view. Submodule instances, and the paths in the instance names of a
//! flattened netlist, become groups. A closed group is drawn as one box whose pins are wired to the outside
//! nets. An opened group is a frame around its own sheet, which may again hold closed boxes.
use crate::netlist::{End, Netlist};
use crate::schematic::{self, GroupText, Node, NodeKind, SchLayout, Wire, BOX_HEAD, FRAME_HEAD, PITCH};
use eframe::egui::{Pos2, Vec2};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// nets with more cell pins than this do not pass a group on to the cells they connect (clocks, resets)
const LINK_FANOUT: usize = 64;
/// space between a frame and its sheet
const FRAME_PAD: f32 = 14.0;

#[derive(Clone, Debug)]
pub struct BoxPin {
    pub name: String,
    pub net: u32,
    pub out: bool,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub path: String,
    /// module name, empty for a group taken from instance names
    pub module: String,
    pub parent: u32,
    pub children: Vec<u32>,
    /// cells directly in this group
    pub cells: Vec<u32>,
    /// cells in this group and below
    pub total: usize,
    pub pins: Vec<BoxPin>,
    /// preorder range: group h is in this group's subtree when tin <= h.tin < tout
    pub tin: u32,
    pub tout: u32,
}

pub struct Fold {
    /// group 0 is the top
    pub groups: Vec<Group>,
    pub cell_group: Vec<u32>,
    /// cells without a hierarchical name that were put into a group by their connections
    pub linked: Vec<bool>,
    /// number of groups taken from instance names (not from submodule instances)
    pub from_names: usize,
}

/// names given by synthesis or by physical design tools, which say nothing about the hierarchy: `_123_`,
/// `$abc$..`, letters followed by a number (`place12`, `u45`, `input3`), and OpenROAD clock tree and
/// repair buffers (`clkbuf_..`, `delaybuf_..`, `load_slew3`, `max_length5`, `rebuffer_mt2`)
pub fn tool_name(s: &str) -> bool {
    let b = s.as_bytes();
    if b.is_empty() || b[0] == b'_' || b[0] == b'$' {
        return true;
    }
    let k = b.iter().take_while(|c| c.is_ascii_alphabetic()).count();
    if k > 0 && k < b.len() && b[k..].iter().all(|c| c.is_ascii_digit()) {
        return true;
    }
    const PRE: [&str; 12] = ["clkbuf_", "clkload", "clone_", "delaybuf_", "load_slew", "max_length", "max_cap", "rebuffer", "ANTENNA", "FILLER", "TAP_", "PHY_"];
    PRE.iter().any(|p| s.starts_with(p) && (p.ends_with('_') || s[p.len()..].starts_with(|c: char| c.is_ascii_digit() || c == '_')))
}

fn new_group(path: String, module: String, parent: u32) -> Group {
    Group { path, module, parent, children: vec![], cells: vec![], total: 0, pins: vec![], tin: 0, tout: 0 }
}

impl Fold {
    pub fn build(nl: &Netlist) -> Fold {
        let mut groups = vec![new_group(String::new(), String::new(), 0)];
        let mut by_path: HashMap<String, u32> = HashMap::new();
        by_path.insert(String::new(), 0);
        let mut hier_of: Vec<Option<usize>> = vec![None];
        // submodule instances (listed parent first)
        for (hi, h) in nl.insts.iter().enumerate() {
            let parent = by_path.get(&h.parent).copied().unwrap_or(0);
            let id = groups.len() as u32;
            groups.push(new_group(h.path.clone(), h.module.clone(), parent));
            groups[parent as usize].children.push(id);
            by_path.insert(h.path.clone(), id);
            hier_of.push(Some(hi));
        }
        // paths in instance names, below the module the cell is in
        let n = nl.cells.len();
        let mut cell_group = vec![0u32; n];
        let mut free = vec![false; n];
        let mut from_names = 0usize;
        for (ci, c) in nl.cells.iter().enumerate() {
            let base = by_path.get(&c.scope).copied().unwrap_or(0);
            let local = if c.scope.is_empty() { c.name.as_str() } else { c.name.strip_prefix(c.scope.as_str()).and_then(|r| r.strip_prefix('/')).unwrap_or(c.name.as_str()) };
            let mut g = base;
            match local.rfind('/') {
                Some(k) if !tool_name(local.split('/').next().unwrap_or("")) => {
                    let bp = &groups[base as usize].path;
                    let full = if bp.is_empty() { local[..k].to_string() } else { format!("{}/{}", bp, &local[..k]) };
                    if let Some(&x) = by_path.get(&full) {
                        g = x;
                    } else {
                        for seg in local[..k].split('/').filter(|x| !x.is_empty()) {
                            let gp = &groups[g as usize].path;
                            let path = if gp.is_empty() { seg.to_string() } else { format!("{}/{}", gp, seg) };
                            g = match by_path.get(&path) {
                                Some(&x) => x,
                                None => {
                                    let id = groups.len() as u32;
                                    groups.push(new_group(path.clone(), String::new(), g));
                                    groups[g as usize].children.push(id);
                                    hier_of.push(None);
                                    by_path.insert(path, id);
                                    from_names += 1;
                                    id
                                }
                            };
                        }
                    }
                }
                _ => free[ci] = tool_name(local) || local.contains('/'),
            }
            cell_group[ci] = g;
        }
        // preorder numbering
        let mut t = 0u32;
        let mut stack = vec![(0u32, false)];
        while let Some((g, done)) = stack.pop() {
            if done {
                groups[g as usize].tout = t;
                continue;
            }
            groups[g as usize].tin = t;
            t += 1;
            stack.push((g, true));
            for &c in groups[g as usize].children.iter().rev() {
                stack.push((c, false));
            }
        }
        // a cell with a tool-given name can only be placed when its module has groups taken from names
        let has_named: Vec<bool> = (0..groups.len()).map(|g| groups[g].children.iter().any(|&c| hier_of[c as usize].is_none())).collect();
        for ci in 0..n {
            if free[ci] && !has_named[cell_group[ci] as usize] {
                free[ci] = false;
            }
        }
        // such cells join the group of the nearest placed cells (fewest net hops, nets up to LINK_FANOUT
        // pins): the group most of them share at that distance, on a tie the one with more named cells.
        // Cells no placed cell reaches stay in their module.
        let mut linked = vec![false; n];
        let mut seeds = vec![0usize; groups.len()];
        for ci in 0..n {
            if !free[ci] {
                seeds[cell_group[ci] as usize] += 1;
            }
        }
        if free.iter().any(|&f| f) {
            let base: Vec<u32> = cell_group.clone();
            let within = |g: u32, a: u32| {
                let (x, y) = (&groups[g as usize], &groups[a as usize]);
                x.tin >= y.tin && x.tin < y.tout
            };
            let mut done: Vec<bool> = free.iter().map(|&f| !f).collect();
            let mut frontier: Vec<u32> = (0..n as u32).filter(|&c| done[c as usize]).collect();
            while !frontier.is_empty() {
                let mut cand: Vec<(u32, u32)> = vec![];
                for &c in &frontier {
                    let g = cell_group[c as usize];
                    for p in &nl.cells[c as usize].pins {
                        let net = &nl.nets[p.net as usize];
                        if net.drivers.len() + net.sinks.len() > LINK_FANOUT {
                            continue;
                        }
                        for e in net.drivers.iter().chain(net.sinks.iter()) {
                            if let End::Cell(o, _) = *e {
                                if !done[o as usize] && within(g, base[o as usize]) {
                                    cand.push((o, g));
                                }
                            }
                        }
                    }
                }
                cand.sort_unstable();
                let mut next = vec![];
                let mut i = 0;
                while i < cand.len() {
                    let o = cand[i].0;
                    let (mut best, mut bc) = (cand[i].1, 0usize);
                    let mut j = i;
                    while j < cand.len() && cand[j].0 == o {
                        let mut k = j;
                        while k < cand.len() && cand[k] == cand[j] {
                            k += 1;
                        }
                        if k - j > bc {
                            bc = k - j;
                            best = cand[j].1;
                        } else if k - j == bc && seeds[cand[j].1 as usize] > seeds[best as usize] {
                            best = cand[j].1;
                        }
                        j = k;
                    }
                    cell_group[o as usize] = best;
                    done[o as usize] = true;
                    linked[o as usize] = true;
                    next.push(o);
                    i = j;
                }
                frontier = next;
            }
        }
        for ci in 0..n {
            groups[cell_group[ci] as usize].cells.push(ci as u32);
        }
        for g in (0..groups.len()).rev() {
            groups[g].total += groups[g].cells.len();
            if g > 0 {
                let (p, t) = (groups[g].parent as usize, groups[g].total);
                groups[p].total += t;
            }
        }
        // pins: the declared ports of a submodule instance. For a group taken from names, every net that
        // has pins both inside and outside it (or a top port), as an output when it is driven inside
        for g in 1..groups.len() {
            if let Some(hi) = hier_of[g] {
                groups[g].pins = nl.insts[hi].ports.iter().map(|(name, dir, net)| BoxPin { name: name.clone(), net: *net, out: dir == "output" }).collect();
            }
        }
        if from_names > 0 {
            let parent: Vec<u32> = groups.iter().map(|g| g.parent).collect();
            let mut acc: Vec<Vec<BoxPin>> = vec![vec![]; groups.len()];
            let mut tmp: Vec<(u32, bool)> = vec![];
            for (ni, net) in nl.nets.iter().enumerate() {
                tmp.clear();
                let (mut ends, mut port) = (0usize, false);
                for (e, drv) in net.drivers.iter().map(|e| (e, true)).chain(net.sinks.iter().map(|e| (e, false))) {
                    match *e {
                        End::Cell(c, _) => {
                            ends += 1;
                            let mut g = cell_group[c as usize];
                            while g != 0 {
                                if hier_of[g as usize].is_none() {
                                    tmp.push((g, drv));
                                }
                                g = parent[g as usize];
                            }
                        }
                        End::Port(_) => port = true,
                        End::Const(_) => {}
                    }
                }
                if tmp.is_empty() {
                    continue;
                }
                tmp.sort_unstable();
                let mut i = 0;
                while i < tmp.len() {
                    let g = tmp[i].0;
                    let mut j = i;
                    let mut drv = false;
                    while j < tmp.len() && tmp[j].0 == g {
                        drv |= tmp[j].1;
                        j += 1;
                    }
                    if j - i < ends || port {
                        acc[g as usize].push(BoxPin { name: net.name.clone(), net: ni as u32, out: drv });
                    }
                    i = j;
                }
            }
            for (g, mut pins) in acc.into_iter().enumerate() {
                if hier_of[g].is_none() && g > 0 {
                    pins.sort_by(|a, b| a.name.cmp(&b.name));
                    groups[g].pins = pins;
                }
            }
        }
        Fold { groups, cell_group, linked, from_names }
    }

    pub fn has_groups(&self) -> bool {
        self.groups.len() > 1
    }

    /// cell or group `g` lies in the subtree of group `a`
    pub fn within(&self, g: u32, a: u32) -> bool {
        let (x, y) = (&self.groups[g as usize], &self.groups[a as usize]);
        x.tin >= y.tin && x.tin < y.tout
    }

    /// nodes drawn directly on a group's sheet
    pub fn items(&self, g: u32) -> usize {
        self.groups[g as usize].cells.len() + self.groups[g as usize].children.len()
    }

    /// the groups from the top down to (and with) `g`, without the top
    pub fn chain(&self, mut g: u32) -> Vec<u32> {
        let mut v = vec![];
        while g != 0 {
            v.push(g);
            g = self.groups[g as usize].parent;
        }
        v.reverse();
        v
    }

    /// + on a closed group opens it and the groups above it, - on an open one closes it and everything
    /// opened inside it. Returns whether the group is open now.
    pub fn toggle(&self, open: &mut HashSet<u32>, g: u32) -> bool {
        if open.contains(&g) {
            open.retain(|&x| !self.within(x, g));
            false
        } else {
            open.extend(self.chain(g));
            true
        }
    }

    pub fn text(&self, g: u32) -> GroupText {
        let gr = &self.groups[g as usize];
        let title = if gr.module.is_empty() { gr.path.rsplit('/').next().unwrap_or("").to_string() } else { gr.module.clone() };
        GroupText { title, path: gr.path.clone(), cells: gr.total }
    }

    /// the closed box of a group: title band, inputs on the left and outputs on the right
    /// The pins of one bus share a row, and so do unnamed pins whose nets join the same nodes (`sig`: net
    /// to the id of the nodes it joins, for nets that share them with another net and reach no loose cell).
    pub fn box_node(&self, g: u32, sig: &HashMap<u32, u32>) -> Node {
        let gr = &self.groups[g as usize];
        // rows of one side: (label, nets)
        let rows_of = |out: bool| -> Vec<(String, Vec<u32>)> {
            let mut rows: Vec<(Vec<&str>, Vec<u32>)> = vec![];
            let mut row_of: HashMap<(u8, String), usize> = HashMap::new();
            for (i, p) in gr.pins.iter().enumerate().filter(|(_, p)| p.out == out) {
                let key = match (schematic::bus_base(&p.name), sig.get(&p.net)) {
                    (Some((b, _)), _) => (0u8, b.to_string()),
                    (None, Some(s)) => (1u8, s.to_string()),
                    (None, None) => (2u8, i.to_string()),
                };
                let r = *row_of.entry(key).or_insert_with(|| {
                    rows.push((vec![], vec![]));
                    rows.len() - 1
                });
                rows[r].0.push(p.name.as_str());
                rows[r].1.push(p.net);
            }
            rows.into_iter().map(|(names, nets)| (schematic::bus_label(&names), nets)).collect()
        };
        let (ins, outs) = (rows_of(false), rows_of(true));
        let rows = ins.len().max(outs.len()).max(1) as f32;
        let h = BOX_HEAD + (rows + 1.0) * PITCH;
        let t = self.text(g);
        let lw = |v: &[(String, Vec<u32>)]| v.iter().map(|r| r.0.chars().count().min(24)).max().unwrap_or(0) as f32 * 5.0;
        // a box with thousands of pins is made wide as well as tall, so it reads as a block from far out
        let w = (t.title.chars().count().min(40) as f32 * 7.0 + 50.0).max(lw(&ins) + lw(&outs) + 36.0).max(100.0).max(h * 0.3) + 16.0;
        let y = |i: usize| BOX_HEAD + (i as f32 + 1.0) * PITCH;
        let pins = |v: &[(String, Vec<u32>)]| -> (Vec<(u32, f32)>, Vec<String>) {
            let mut p = vec![];
            let mut names = vec![];
            for (i, (label, nets)) in v.iter().enumerate() {
                for &n in nets {
                    p.push((n, y(i)));
                    names.push(label.clone());
                }
            }
            (p, names)
        };
        let ((pi, ni), (po, no)) = (pins(&ins), pins(&outs));
        Node { kind: NodeKind::Block(g), w, h, ins: pi, outs: po, pin_names_in: ni, pin_names_out: no, layer: 0, pos: Pos2::ZERO }
    }

    /// nets on the closed boxes of sheet `g` that join exactly the same nodes as another net (boxes, the
    /// outside, never a loose cell): net -> id of those nodes
    fn shared_ends(&self, nl: &Netlist, g: u32, open: &HashSet<u32>) -> HashMap<u32, u32> {
        let gr = &self.groups[g as usize];
        // the node of this sheet a cell is drawn in: 0 the cell itself, 1 a child box, 2 outside
        let node_of = |c: u32| -> (u8, u32) {
            let mut x = self.cell_group[c as usize];
            if x == g {
                return (0, c);
            }
            if !self.within(x, g) {
                return (2, 0);
            }
            while self.groups[x as usize].parent != g {
                x = self.groups[x as usize].parent;
            }
            (1, x)
        };
        let mut seen: HashSet<u32> = HashSet::new();
        let mut ends_of: Vec<(u32, Vec<(u8, u32, bool)>)> = vec![];
        for &ch in gr.children.iter().filter(|c| !open.contains(c)) {
            for p in &self.groups[ch as usize].pins {
                if !seen.insert(p.net) {
                    continue;
                }
                let net = &nl.nets[p.net as usize];
                let mut e: Vec<(u8, u32, bool)> = vec![];
                for (x, drv) in net.drivers.iter().map(|x| (x, true)).chain(net.sinks.iter().map(|x| (x, false))) {
                    match *x {
                        End::Cell(c, _) => {
                            let (k, id) = node_of(c);
                            e.push((k, id, drv));
                        }
                        End::Port(_) => e.push((2, 0, drv)),
                        End::Const(_) => {}
                    }
                }
                e.sort_unstable();
                e.dedup();
                if !e.iter().any(|x| x.0 == 0) {
                    ends_of.push((p.net, e));
                }
            }
        }
        let mut count: HashMap<&Vec<(u8, u32, bool)>, (u32, usize)> = HashMap::new();
        for (_, e) in &ends_of {
            let n = count.len() as u32;
            count.entry(e).or_insert((n, 0)).1 += 1;
        }
        ends_of.iter().filter_map(|(net, e)| count.get(e).filter(|c| c.1 > 1).map(|c| (*net, c.0))).collect()
    }
}

/// a laid-out sheet of one group with everything opened below it. For a group other than the top, in frame
/// coordinates (the frame from (0, 0) to `lay.size`) with the frame pins on its left and right edges
pub struct Level {
    pub lay: SchLayout,
    pub ins: Vec<(u32, f32)>,
    pub outs: Vec<(u32, f32)>,
}

/// sheets already laid out, by group and the opened groups below it
pub type Cache = HashMap<(u32, Vec<u32>), Arc<Level>>;

fn open_below(f: &Fold, g: u32, open: &HashSet<u32>) -> Vec<u32> {
    let mut v: Vec<u32> = open.iter().copied().filter(|&x| x != g && f.within(x, g)).collect();
    v.sort_unstable();
    v
}

fn level(nl: &Netlist, f: &Fold, g: u32, open: &HashSet<u32>, cache: &mut Cache) -> Arc<Level> {
    let key = (g, open_below(f, g, open));
    if let Some(l) = cache.get(&key) {
        return l.clone();
    }
    let gr = &f.groups[g as usize];
    let mut nodes: Vec<Node> = gr.cells.iter().map(|&c| schematic::cell_node(nl, c)).collect();
    let mut subs: HashMap<u32, Arc<Level>> = HashMap::new();
    // buses and nets running between the same nodes are bundled on folded sheets
    let bundle = f.has_groups();
    let sig = if bundle { f.shared_ends(nl, g, open) } else { HashMap::new() };
    for &ch in &gr.children {
        if open.contains(&ch) {
            let sub = level(nl, f, ch, open, cache);
            nodes.push(Node { kind: NodeKind::Frame(ch), w: sub.lay.size.x, h: sub.lay.size.y, ins: sub.ins.clone(), outs: sub.outs.clone(), pin_names_in: vec![], pin_names_out: vec![], layer: 0, pos: Pos2::ZERO });
            subs.insert(ch, sub);
        } else {
            nodes.push(f.box_node(ch, &sig));
        }
    }
    let inside = |c: u32| f.within(f.cell_group[c as usize], g);
    let mut lay = schematic::layout_nodes(nl, nodes, &inside, g == 0, g != 0, bundle);
    for &ch in &gr.children {
        lay.labels.insert(ch, f.text(ch));
    }
    let frames: Vec<(usize, u32)> = lay.nodes.iter().enumerate().filter_map(|(i, n)| if let NodeKind::Frame(ch) = n.kind { Some((i, ch)) } else { None }).collect();
    // nodes of this sheet itself, the sheets of opened children come after them
    let own = lay.nodes.len();
    for (i, ch) in frames {
        let off = lay.nodes[i].pos.to_vec2();
        schematic::append(&mut lay, &subs[&ch].lay, off);
    }
    let (mut ins, mut outs) = (vec![], vec![]);
    if g != 0 {
        // frame around the sheet, the stubs of nets crossing it extended to its edges
        let (mut a, mut b) = (Pos2::new(f32::INFINITY, f32::INFINITY), Pos2::new(f32::NEG_INFINITY, f32::NEG_INFINITY));
        for n in lay.nodes.iter().filter(|n| !matches!(n.kind, NodeKind::Dummy)) {
            a = a.min(n.pos);
            b = b.max(n.pos + Vec2::new(n.w, n.h));
        }
        for w in &lay.wires {
            for p in &w.pts {
                a = a.min(*p);
                b = b.max(*p);
            }
        }
        if !a.x.is_finite() {
            a = Pos2::ZERO;
            b = Pos2::new(120.0, 40.0);
        }
        let x0 = a.x - FRAME_PAD;
        let x1 = b.x + FRAME_PAD;
        let y0 = a.y - FRAME_HEAD - FRAME_PAD;
        let y1 = b.y + FRAME_PAD;
        let mut edges = vec![];
        for n in &lay.nodes[..own] {
            match n.kind {
                // a stub may stand for several nets: all of them become pins of the frame at its height
                NodeKind::InStub(net) => {
                    let y = n.pos.y + n.outs[0].1;
                    edges.push(Wire { net, pts: vec![Pos2::new(x0, y), Pos2::new(n.pos.x, y)], bundle: n.outs.len() > 1 });
                    ins.extend(n.outs.iter().map(|x| (x.0, y - y0)));
                }
                NodeKind::OutStub(net) => {
                    let y = n.pos.y + n.ins[0].1;
                    edges.push(Wire { net, pts: vec![Pos2::new(n.pos.x + n.w, y), Pos2::new(x1, y)], bundle: n.ins.len() > 1 });
                    outs.extend(n.ins.iter().map(|x| (x.0, y - y0)));
                }
                _ => {}
            }
        }
        lay.wires.extend(edges);
        let off = Vec2::new(-x0, -y0);
        for n in lay.nodes.iter_mut() {
            n.pos += off;
        }
        for w in lay.wires.iter_mut() {
            for p in w.pts.iter_mut() {
                *p += off;
            }
        }
        for d in lay.dots.iter_mut() {
            d.1 += off;
        }
        lay.size = Vec2::new(x1 - x0, y1 - y0);
    }
    let l = Arc::new(Level { lay, ins, outs });
    cache.insert(key, l.clone());
    l
}

/// the whole design folded: the top sheet with the opened groups drawn in their frames
pub fn view(nl: &Netlist, f: &Fold, open: &HashSet<u32>, cache: &mut Cache) -> SchLayout {
    let top = level(nl, f, 0, open, cache);
    let mut lay = SchLayout { nodes: vec![], wires: vec![], dots: vec![], size: top.lay.size, cell_node: HashMap::new(), group_node: HashMap::new(), labels: HashMap::new(), bundles: vec![] };
    schematic::append(&mut lay, &top.lay, Vec2::ZERO);
    // keep only the sheets this view still uses
    let keep: HashSet<(u32, Vec<u32>)> = std::iter::once(0).chain(open.iter().copied()).map(|g| (g, open_below(f, g, open))).collect();
    cache.retain(|k, _| keep.contains(k));
    lay
}
