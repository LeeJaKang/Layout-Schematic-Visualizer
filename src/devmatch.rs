//! Match extracted layout devices to CDL devices (an LVS-style comparison of one cell), port of devmatch.js.
//! Both sides are reduced to series-parallel trees the way LVS reduces them: devices between the same two
//! nets combine in parallel (identical structures add their fins), chains through internal nodes combine in
//! series, and child order inside a series or parallel node does not matter. Label names seed the net
//! mapping, and a backtracking search pairs the reduced devices so the net mapping stays one-to-one.
//! Afterwards every CDL device and net is traced back to the layout fingers and nets to highlight.
use crate::cdl::Subckt;
use crate::extract::Extraction;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
enum Node {
    /// w (summed over parallel members) and l in metres, both 0 when sizes are not compared
    L { g: i64, fins: f64, w: f64, l: f64, members: Vec<usize> },
    S(Vec<Node>),
    P(Vec<Node>),
}

fn sig(n: &Node) -> String {
    match n {
        Node::L { g, l, .. } => if *l > 0.0 { format!("L{}/{}", g, (l * 1e11).round() as i64) } else { format!("L{}", g) },
        Node::S(ch) | Node::P(ch) => {
            let mut v: Vec<String> = ch.iter().map(sig).collect();
            v.sort();
            format!("{}({})", if matches!(n, Node::S(_)) { "S" } else { "P" }, v.join(","))
        }
    }
}
fn canon(n: &mut Node) {
    if let Node::S(ch) | Node::P(ch) = n {
        ch.iter_mut().for_each(canon);
        ch.sort_by_cached_key(sig);
    }
}
fn add_fins(into: &mut Node, from: &Node) {
    match (into, from) {
        (Node::L { fins, w, members, .. }, Node::L { fins: f2, w: w2, members: m2, .. }) => {
            *fins += f2;
            *w += w2;
            members.extend_from_slice(m2);
        }
        (Node::S(a), Node::S(b)) | (Node::P(a), Node::P(b)) => {
            for (x, y) in a.iter_mut().zip(b) {
                add_fins(x, y);
            }
        }
        _ => {}
    }
}
fn gates_of(n: &Node, out: &mut HashSet<i64>) {
    match n {
        Node::L { g, .. } => {
            out.insert(*g);
        }
        Node::S(ch) | Node::P(ch) => ch.iter().for_each(|c| gates_of(c, out)),
    }
}
fn leaves<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    match n {
        Node::L { .. } => out.push(n),
        Node::S(ch) | Node::P(ch) => ch.iter().for_each(|c| leaves(c, out)),
    }
}
fn combine(series: bool, nodes: Vec<Node>) -> Node {
    let mut ch = vec![];
    for n in nodes {
        match (series, n) {
            (true, Node::S(c)) => ch.extend(c),
            (false, Node::P(c)) => ch.extend(c),
            (_, other) => ch.push(other),
        }
    }
    if series { Node::S(ch) } else { Node::P(ch) }
}

#[derive(Clone, Debug)]
struct Dev {
    typ: String,
    bulk: i64,
    ends: [i64; 2],
    node: Node,
}

fn reduce(mut list: Vec<Dev>, is_internal: &dyn Fn(i64) -> bool) -> Vec<Dev> {
    for _round in 0..200 {
        let mut changed = false;
        // parallel: same type, bulk and end nets
        let mut order: Vec<(String, i64, [i64; 2])> = vec![];
        let mut groups: HashMap<(String, i64, [i64; 2]), Vec<Dev>> = HashMap::new();
        for mut d in list {
            d.ends.sort();
            let key = (d.typ.clone(), d.bulk, d.ends);
            if !groups.contains_key(&key) {
                order.push(key.clone());
            }
            groups.entry(key).or_default().push(d);
        }
        let mut next = vec![];
        for key in order {
            let g = groups.remove(&key).unwrap();
            if g.len() == 1 {
                next.extend(g);
                continue;
            }
            changed = true;
            let mut sig_order: Vec<String> = vec![];
            let mut by_sig: HashMap<String, Dev> = HashMap::new();
            for mut d in g {
                canon(&mut d.node);
                let s = sig(&d.node);
                match by_sig.get_mut(&s) {
                    Some(e) => add_fins(&mut e.node, &d.node),
                    None => {
                        sig_order.push(s.clone());
                        by_sig.insert(s, d);
                    }
                }
            }
            let mut parts: Vec<Dev> = sig_order.into_iter().map(|s| by_sig.remove(&s).unwrap()).collect();
            if parts.len() == 1 {
                next.push(parts.pop().unwrap());
            } else {
                let (typ, bulk, ends) = (parts[0].typ.clone(), parts[0].bulk, parts[0].ends);
                next.push(Dev { typ, bulk, ends, node: combine(false, parts.into_iter().map(|p| p.node).collect()) });
            }
        }
        list = next;
        // series: an internal node used by exactly two device ends and no gate
        let mut uses_order: Vec<i64> = vec![];
        let mut uses: HashMap<i64, Vec<usize>> = HashMap::new();
        let mut gate_use = HashSet::new();
        for (i, d) in list.iter().enumerate() {
            for &e in &d.ends {
                if !uses.contains_key(&e) {
                    uses_order.push(e);
                }
                uses.entry(e).or_default().push(i);
            }
            gates_of(&d.node, &mut gate_use);
        }
        let mut dead: HashSet<usize> = HashSet::new();
        for nn in uses_order {
            let u = &uses[&nn];
            if u.len() != 2 || u[0] == u[1] || !is_internal(nn) || gate_use.contains(&nn) || dead.contains(&u[0]) || dead.contains(&u[1]) {
                continue;
            }
            let (ia, ib) = (u[0], u[1]);
            if list[ia].typ != list[ib].typ || list[ia].bulk != list[ib].bulk {
                continue;
            }
            let a_ends = list[ia].ends;
            let b_ends = list[ib].ends;
            let ea = if a_ends[0] == nn { a_ends[1] } else { a_ends[0] };
            let eb = if b_ends[0] == nn { b_ends[1] } else { b_ends[0] };
            if ea == eb {
                continue;
            }
            let bnode = list[ib].node.clone();
            let anode = std::mem::replace(&mut list[ia].node, Node::S(vec![]));
            list[ia].ends = [ea, eb];
            list[ia].node = combine(true, vec![anode, bnode]);
            dead.insert(ib);
            changed = true;
        }
        if !dead.is_empty() {
            list = list.into_iter().enumerate().filter(|(i, _)| !dead.contains(i)).map(|(_, d)| d).collect();
        }
        if !changed {
            break;
        }
    }
    for d in list.iter_mut() {
        canon(&mut d.node);
    }
    list
}

#[derive(Default, Debug)]
pub struct MatchResult {
    pub ok: bool,
    pub steps: usize,
    pub conflicts: Vec<String>,
    pub layout_groups: usize,
    pub cdl_groups: usize,
    pub matched_groups: usize,
    pub unmatched_layout: Vec<String>,
    pub unmatched_cdl: Vec<String>,
    /// CDL device name -> layout finger indices
    pub dev_to_layout: HashMap<String, Vec<usize>>,
    /// layout finger -> CDL device names
    pub layout_to_dev: HashMap<usize, Vec<String>>,
    /// CDL net -> layout nets
    pub net_to_layout: HashMap<String, Vec<usize>>,
    pub layout_to_net: HashMap<usize, String>,
    /// layout net -> CDL net
    pub net_map: HashMap<usize, String>,
    /// CDL devices in symmetric groups: any of several layout copies matches, the one shown is picked by order
    pub symmetric_devs: Vec<String>,
    /// CDL nets of those groups
    pub symmetric_nets: Vec<String>,
    /// likely causes of a mismatch in plain words (pin texts against CDL ports, malformed CDL lines)
    pub causes: Vec<String>,
}

struct Search {
    nmap: HashMap<i64, i64>,
    rmap: HashMap<i64, i64>,
    undo: Vec<i64>,
    leaf_pairs: Vec<(Vec<usize>, Vec<usize>)>,
    use_fins: bool,
    nodes: usize,
    abort: bool,
}

impl Search {
    fn bind(&mut self, a: i64, c: i64) -> bool {
        if a < 0 {
            return false;
        }
        if let Some(&x) = self.nmap.get(&a) {
            return x == c;
        }
        if self.rmap.contains_key(&c) {
            return false;
        }
        self.nmap.insert(a, c);
        self.rmap.insert(c, a);
        self.undo.push(a);
        true
    }
    fn rollback(&mut self, mark: usize, pmark: usize) {
        while self.undo.len() > mark {
            let a = self.undo.pop().unwrap();
            if let Some(c) = self.nmap.remove(&a) {
                self.rmap.remove(&c);
            }
        }
        self.leaf_pairs.truncate(pmark);
    }
    /// match l against c, then run the continuation k on the resulting bindings; on failure every
    /// alternative pairing inside the series/parallel trees is tried before giving up, and the bindings
    /// are rolled back. On success the bindings stay.
    fn match_node(&mut self, l: &Node, c: &Node, k: &mut dyn FnMut(&mut Search) -> bool) -> bool {
        self.nodes += 1;
        if self.nodes > NODE_LIMIT {
            self.abort = true;
        }
        if self.abort {
            return false;
        }
        match (l, c) {
            (Node::L { g: lg, fins: lf, w: lw, l: ll, members: lm }, Node::L { g: cg, fins: cf, w: cw, l: cl, members: cm }) => {
                if self.use_fins && lf != cf {
                    return false;
                }
                if self.use_fins && (!size_eq(*lw, *cw) || !size_eq(*ll, *cl)) {
                    return false;
                }
                let (m, p) = (self.undo.len(), self.leaf_pairs.len());
                if !self.bind(*lg, *cg) {
                    return false;
                }
                self.leaf_pairs.push((lm.clone(), cm.clone()));
                if k(self) {
                    return true;
                }
                self.rollback(m, p);
                false
            }
            (Node::S(a), Node::S(b)) | (Node::P(a), Node::P(b)) => {
                if a.len() != b.len() {
                    return false;
                }
                let mut used = vec![false; b.len()];
                self.go(a, b, 0, &mut used, k)
            }
            _ => false,
        }
    }
    fn go(&mut self, a: &[Node], b: &[Node], i: usize, used: &mut Vec<bool>, k: &mut dyn FnMut(&mut Search) -> bool) -> bool {
        if i == a.len() {
            return k(self);
        }
        for j in 0..b.len() {
            if used[j] || self.abort {
                continue;
            }
            used[j] = true;
            let ok = self.match_node(&a[i], &b[j], &mut |s: &mut Search| s.go(a, b, i + 1, used, k));
            used[j] = false;
            if ok {
                return true;
            }
        }
        false
    }
    fn try_pair(&mut self, l: &Dev, c: &Dev, k: &mut dyn FnMut(&mut Search) -> bool) -> bool {
        if l.typ != c.typ {
            return false;
        }
        for order in [[0usize, 1usize], [1, 0]] {
            let (m, p) = (self.undo.len(), self.leaf_pairs.len());
            if self.bind(l.ends[0], c.ends[order[0]]) && self.bind(l.ends[1], c.ends[order[1]]) && (l.bulk < 0 || self.bind(l.bulk, c.bulk)) && self.match_node(&l.node, &c.node, k) {
                return true;
            }
            self.rollback(m, p);
        }
        false
    }
}

/// tree node comparisons per cell before the search gives up
const NODE_LIMIT: usize = 20_000_000;

/// W or L equal within 0.05% (layout sizes are exact grid sums, CDL values are rounded decimals)
fn size_eq(a: f64, b: f64) -> bool {
    (a - b).abs() <= 5e-4 * a.abs().max(b.abs())
}

struct Best {
    done: usize,
    pair: Vec<i64>,
    nmap: HashMap<i64, i64>,
    leaf_pairs: Vec<(Vec<usize>, Vec<usize>)>,
}

/// total channel width of a CDL device in metres: w * m (SPICE w is already the width over all nf/ng fingers)
pub fn cdl_w(d: &crate::cdl::Mos, scale: f64) -> f64 {
    let m = if d.m == 0.0 || d.m.is_nan() { 1.0 } else { d.m };
    if d.w.is_finite() { d.w * m * scale } else { 0.0 }
}
pub fn cdl_l(d: &crate::cdl::Mos, scale: f64) -> f64 {
    if d.l.is_finite() { d.l * scale } else { 0.0 }
}

pub fn match_cell(ex: &Extraction, sc: &Subckt, use_fins: bool) -> MatchResult {
    // CDL nets as negative-free integer ids
    let mut cid: HashMap<String, i64> = HashMap::new();
    let mut cname: Vec<String> = vec![];
    let mut intern = |s: &str| -> i64 {
        if let Some(&i) = cid.get(s) {
            return i;
        }
        let i = cname.len() as i64;
        cname.push(s.to_string());
        cid.insert(s.to_string(), i);
        i
    };
    for p in &sc.ports {
        intern(p);
    }
    for d in &sc.mos {
        for x in [&d.d, &d.g, &d.s, &d.b] {
            intern(x);
        }
    }
    let ports: HashSet<i64> = sc.ports.iter().map(|p| cid[p]).collect();
    let mut conflicts = vec![];
    let mut s = Search { nmap: HashMap::new(), rmap: HashMap::new(), undo: vec![], leaf_pairs: vec![], use_fins, nodes: 0, abort: false };
    let n_cnets = cname.len();
    for n in &ex.nets {
        let hits: Vec<&String> = n.names.iter().filter(|x| cid.contains_key(*x)).collect();
        if hits.is_empty() {
            continue;
        }
        if hits.len() > 1 {
            conflicts.push(format!("one layout net carries the labels {}, which are different CDL nets", hits.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
        }
        let c = cid[hits[0]];
        if s.rmap.contains_key(&c) {
            conflicts.push(format!("label {} is on two separate layout nets", hits[0]));
            continue;
        }
        s.nmap.insert(n.id as i64, c);
        s.rmap.insert(c, n.id as i64);
    }
    let port_names: HashSet<&String> = sc.ports.iter().collect();
    let layout_port_net = |id: i64| id >= 0 && ex.nets.get(id as usize).map_or(false, |n| n.names.iter().any(|x| port_names.contains(x)));
    let ldevs: Vec<Dev> = ex
        .devices
        .iter()
        .enumerate()
        .filter(|(_, d)| d.gate >= 0 && d.sd.len() == 2)
        .map(|(i, d)| Dev { typ: d.typ.clone(), bulk: d.bulk, ends: [d.sd[0] as i64, d.sd[1] as i64], node: Node::L { g: d.gate, fins: d.nfin, w: if ex.size_wl { d.w } else { 0.0 }, l: if ex.size_wl { d.l } else { 0.0 }, members: vec![i] } })
        .collect();
    let cdevs: Vec<Dev> = sc
        .mos
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let nfin = if d.nfin.is_finite() { d.nfin } else { 0.0 };
            let m = if d.m == 0.0 || d.m.is_nan() { 1.0 } else { d.m };
            let nf = if d.nf == 0.0 || d.nf.is_nan() { 1.0 } else { d.nf };
            let (w, l) = if ex.size_wl { (cdl_w(d, ex.cdl_scale), cdl_l(d, ex.cdl_scale)) } else { (0.0, 0.0) };
            Dev { typ: d.typ.clone(), bulk: cid[&d.b], ends: [cid[&d.d], cid[&d.s]], node: Node::L { g: cid[&d.g], fins: nfin * m * nf, w, l, members: vec![i] } }
        })
        .collect();
    let lg = reduce(ldevs, &|id| !layout_port_net(id));
    let cg = reduce(cdevs, &|n| !ports.contains(&n));
    let seed: Vec<(i64, i64)> = s.nmap.iter().map(|(&a, &c)| (a, c)).collect();
    drop(s);

    const LIMIT: usize = 300_000;
    fn solve(done: usize, s: &mut Search, lg: &[Dev], cg: &[Dev], used_c: &mut Vec<bool>, pair: &mut Vec<i64>, steps: &mut usize, best: &mut Option<Best>) -> bool {
        *steps += 1;
        if *steps > LIMIT {
            s.abort = true;
        }
        if s.abort {
            return false;
        }
        if done == lg.len() {
            return true;
        }
        let mut bi = usize::MAX;
        let mut bc: Option<Vec<usize>> = None;
        for li in 0..lg.len() {
            if pair[li] >= 0 {
                continue;
            }
            let mut c = vec![];
            for ci in 0..cg.len() {
                if used_c[ci] {
                    continue;
                }
                let (m, p) = (s.undo.len(), s.leaf_pairs.len());
                if s.try_pair(&lg[li], &cg[ci], &mut |_| true) {
                    c.push(ci);
                }
                s.rollback(m, p);
            }
            let better = bc.as_ref().map_or(true, |b| c.len() < b.len());
            if better {
                bi = li;
                let empty = c.is_empty();
                bc = Some(c);
                if empty {
                    break;
                }
            }
        }
        let bc = bc.unwrap_or_default();
        if bc.is_empty() {
            if best.as_ref().map_or(true, |b| done > b.done) {
                *best = Some(Best { done, pair: pair.clone(), nmap: s.nmap.clone(), leaf_pairs: s.leaf_pairs.clone() });
            }
            return false;
        }
        for ci in bc {
            let (m, p) = (s.undo.len(), s.leaf_pairs.len());
            // every consistent internal pairing of (bi, ci) is tried with the rest of the search
            let ok = s.try_pair(&lg[bi], &cg[ci], &mut |s2: &mut Search| {
                pair[bi] = ci as i64;
                used_c[ci] = true;
                if solve(done + 1, s2, lg, cg, used_c, pair, steps, best) {
                    return true;
                }
                pair[bi] = -1;
                used_c[ci] = false;
                false
            });
            if ok {
                return true;
            }
            s.rollback(m, p);
        }
        false
    }
    /// one complete search from the given fixed net bindings: (solved, steps, pair, net map, leaf pairs)
    fn run_search(seed: &[(i64, i64)], lg: &[Dev], cg: &[Dev], use_fins: bool) -> (bool, usize, Vec<i64>, HashMap<i64, i64>, Vec<(Vec<usize>, Vec<usize>)>) {
        let mut s = Search { nmap: HashMap::new(), rmap: HashMap::new(), undo: vec![], leaf_pairs: vec![], use_fins, nodes: 0, abort: false };
        for &(a, c) in seed {
            s.nmap.insert(a, c);
            s.rmap.insert(c, a);
        }
        let mut used_c = vec![false; cg.len()];
        let mut pair = vec![-1i64; lg.len()];
        let mut steps = 0usize;
        let mut best: Option<Best> = None;
        let solved = solve(0, &mut s, lg, cg, &mut used_c, &mut pair, &mut steps, &mut best) && lg.len() == cg.len();
        if !solved && best.is_some() {
            let b = best.unwrap();
            (solved, steps, b.pair, b.nmap, b.leaf_pairs)
        } else {
            (solved, steps, pair, s.nmap, s.leaf_pairs)
        }
    }
    let (solved, steps, mut pp, mut mm, mut lp) = run_search(&seed, &lg, &cg, use_fins);
    // symmetric circuits: every copy matches, so pick by order and say so
    let mut sym_nets: Vec<i64> = vec![];
    let mut sym_devs: HashSet<usize> = HashSet::new();
    if solved && conflicts.is_empty() && use_fins {
        let fxy: Vec<(i32, i32)> = ex.devices.iter().map(|d| (d.rects.iter().map(|r| r[0]).min().unwrap_or(i32::MAX), d.rects.iter().map(|r| r[1]).min().unwrap_or(i32::MAX))).collect();
        let csize: Vec<(f64, f64, f64)> = sc.mos.iter().map(|d| {
            let nfin = if d.nfin.is_finite() { d.nfin } else { 0.0 };
            let m = if d.m == 0.0 || d.m.is_nan() { 1.0 } else { d.m };
            let nf = if d.nf == 0.0 || d.nf.is_nan() { 1.0 } else { d.nf };
            let (w, l) = if ex.size_wl { (cdl_w(d, ex.cdl_scale), cdl_l(d, ex.cdl_scale)) } else { (0.0, 0.0) };
            (nfin * m * nf, w, l)
        }).collect();
        let cdev_nets: Vec<[i64; 4]> = sc.mos.iter().map(|d| [cid[&d.d], cid[&d.g], cid[&d.s], cid[&d.b]]).collect();
        let anchored: HashSet<i64> = seed.iter().map(|&(_, c)| c).collect();
        let classes = net_classes(&sc.mos, &cdev_nets, &csize, n_cnets, &anchored);
        let srch = |sd: &[(i64, i64)]| run_search(sd, &lg, &cg, use_fins);
        let (kn, kd, km) = tie_break(&classes, &seed, &srch, ex, &fxy, &cdev_nets, &mm);
        sym_nets = kn;
        sym_devs.extend(kd);
        if let Some((p2, m2, l2)) = km {
            pp = p2;
            mm = m2;
            lp = l2;
        }
        let (lp2, amb) = split_merged(ex, &mm, &cdev_nets, &csize, &lp, &fxy);
        lp = lp2;
        sym_devs.extend(amb);
    }

    let mut r = MatchResult { steps, layout_groups: lg.len(), cdl_groups: cg.len(), ..Default::default() };
    sym_nets.sort_by_key(|&c| cname[c as usize].clone());
    r.symmetric_nets = sym_nets.iter().map(|&c| cname[c as usize].clone()).collect();
    let mut sd: Vec<usize> = sym_devs.into_iter().collect();
    sd.sort();
    r.symmetric_devs = sd.iter().map(|&i| sc.mos[i].name.clone()).collect();
    for (lm, cm) in &lp {
        for &ci in cm {
            r.dev_to_layout.entry(sc.mos[ci].name.clone()).or_default().extend_from_slice(lm);
        }
        for &f in lm {
            r.layout_to_dev.insert(f, cm.iter().map(|&ci| sc.mos[ci].name.clone()).collect());
        }
    }
    for (&a, &c) in &mm {
        r.net_to_layout.insert(cname[c as usize].clone(), vec![a as usize]);
        r.net_map.insert(a as usize, cname[c as usize].clone());
    }
    // internal nodes through the fingers of the CDL devices on them
    let mut sd_order: Vec<String> = vec![];
    let mut sd_of: HashMap<String, Vec<String>> = HashMap::new();
    for d in &sc.mos {
        for nn in [&d.d, &d.s] {
            if !sd_of.contains_key(nn) {
                sd_order.push(nn.clone());
            }
            sd_of.entry(nn.clone()).or_default().push(d.name.clone());
        }
    }
    for nn in sd_order {
        if r.net_to_layout.contains_key(&nn) {
            continue;
        }
        let mut cand: Option<Vec<usize>> = None;
        for nm in &sd_of[&nn] {
            let mut set: Vec<usize> = vec![];
            for &f in r.dev_to_layout.get(nm).map(|v| v.as_slice()).unwrap_or(&[]) {
                for &x in &ex.devices[f].sd {
                    if !mm.contains_key(&(x as i64)) && !set.contains(&x) {
                        set.push(x);
                    }
                }
            }
            cand = Some(match cand {
                None => set,
                Some(c) => c.into_iter().filter(|x| set.contains(x)).collect(),
            });
        }
        if let Some(c) = cand {
            if !c.is_empty() {
                r.net_to_layout.insert(nn, c);
            }
        }
    }
    for (c, arr) in &r.net_to_layout {
        for &a in arr {
            r.layout_to_net.entry(a).or_insert_with(|| c.clone());
        }
    }
    let paired: HashSet<i64> = pp.iter().copied().filter(|&x| x >= 0).collect();
    r.matched_groups = paired.len();
    for (i, d) in lg.iter().enumerate() {
        if pp[i] < 0 {
            let mut lv = vec![];
            leaves(&d.node, &mut lv);
            r.unmatched_layout.push(format!("{} {} ({} fingers)", d.typ, sig(&d.node), lv.len()));
        }
    }
    for (i, d) in cg.iter().enumerate() {
        if !paired.contains(&(i as i64)) {
            let mut lv = vec![];
            leaves(&d.node, &mut lv);
            let names: Vec<String> = lv.iter().flat_map(|x| if let Node::L { members, .. } = x { members.iter().map(|&ci| sc.mos[ci].name.clone()).collect::<Vec<_>>() } else { vec![] }).collect();
            r.unmatched_cdl.push(format!("{} {}-{}: {}", d.typ, cname[d.ends[0] as usize], cname[d.ends[1] as usize], names.join(" ")));
        }
    }
    r.ok = solved && conflicts.is_empty();
    if !r.ok {
        r.causes = pin_causes(ex, sc);
    }
    r.conflicts = conflicts;
    r
}

type Found = (bool, usize, Vec<i64>, HashMap<i64, i64>, Vec<(Vec<usize>, Vec<usize>)>);

fn hash_of<T: std::hash::Hash>(t: &T) -> u64 {
    use std::hash::Hasher;
    let mut s = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut s);
    s.finish()
}

/// colour refinement over CDL devices and nets from the label-anchored nets: nets that keep one colour are
/// not told apart by connectivity, labels or sizes (candidates for symmetric copies)
fn net_classes(mos: &[crate::cdl::Mos], nets: &[[i64; 4]], size: &[(f64, f64, f64)], n: usize, anchored: &HashSet<i64>) -> Vec<Vec<i64>> {
    let mut nc: Vec<u64> = (0..n as i64).map(|c| if anchored.contains(&c) { hash_of(&("label", c)) } else { hash_of(&"free") }).collect();
    let mut dc: Vec<u64> = mos.iter().zip(size).map(|(d, s)| hash_of(&(d.typ.as_str(), (s.0 * 1e3).round() as i64, (s.1 * 1e12).round() as i64, (s.2 * 1e12).round() as i64))).collect();
    let count = |v: &[u64]| v.iter().collect::<HashSet<_>>().len();
    let mut prev = count(&nc) + count(&dc);
    for _ in 0..(n + mos.len() + 1) {
        dc = (0..mos.len())
            .map(|i| {
                let x = nets[i];
                let mut sd = [nc[x[0] as usize], nc[x[2] as usize]];
                sd.sort();
                hash_of(&(dc[i], nc[x[1] as usize], sd, nc[x[3] as usize]))
            })
            .collect();
        let mut inc: Vec<Vec<(u8, u64)>> = vec![vec![]; n];
        for (i, x) in nets.iter().enumerate() {
            inc[x[1] as usize].push((0, dc[i]));
            inc[x[0] as usize].push((1, dc[i]));
            inc[x[2] as usize].push((1, dc[i]));
            inc[x[3] as usize].push((2, dc[i]));
        }
        nc = (0..n)
            .map(|c| {
                let mut v = std::mem::take(&mut inc[c]);
                v.sort();
                hash_of(&(nc[c], v))
            })
            .collect();
        let now = count(&nc) + count(&dc);
        if now == prev {
            break;
        }
        prev = now;
    }
    let mut order = vec![];
    let mut by: HashMap<u64, Vec<i64>> = HashMap::new();
    for c in 0..n as i64 {
        if anchored.contains(&c) {
            continue;
        }
        let k = nc[c as usize];
        if !by.contains_key(&k) {
            order.push(k);
        }
        by.entry(k).or_default().push(c);
    }
    order.into_iter().map(|k| by.remove(&k).unwrap()).filter(|v| v.len() > 1).collect()
}

/// symmetric net classes: pair the CDL nets in order of first use with the layout nets left to right, and
/// mark a class only when a second pairing also matches. Returns (marked nets, their devices, new match)
fn tie_break(classes: &[Vec<i64>], seed: &[(i64, i64)], search: &dyn Fn(&[(i64, i64)]) -> Found, ex: &Extraction, fxy: &[(i32, i32)], cdev_nets: &[[i64; 4]], mm0: &HashMap<i64, i64>) -> (Vec<i64>, Vec<usize>, Option<(Vec<i64>, HashMap<i64, i64>, Vec<(Vec<usize>, Vec<usize>)>)>) {
    let rm0: HashMap<i64, i64> = mm0.iter().map(|(&a, &c)| (c, a)).collect();
    // only nets the reduction keeps are bound by the search
    let classes: Vec<Vec<i64>> = classes.iter().map(|k| k.iter().copied().filter(|c| rm0.contains_key(c)).collect::<Vec<_>>()).filter(|k| k.len() > 1).collect();
    if classes.is_empty() {
        return (vec![], vec![], None);
    }
    let mut first: HashMap<i64, usize> = HashMap::new();
    for (i, n) in cdev_nets.iter().enumerate() {
        for &c in &n[..3] {
            first.entry(c).or_insert(i);
        }
    }
    let mut geo: HashMap<i64, (i32, i32)> = HashMap::new();
    for (f, d) in ex.devices.iter().enumerate() {
        for a in std::iter::once(d.gate).chain(d.sd.iter().map(|&x| x as i64)) {
            let e = geo.entry(a).or_insert((i32::MAX, i32::MAX));
            if fxy[f] < *e {
                *e = fxy[f];
            }
        }
    }
    let pref: Vec<Vec<(i64, i64)>> = classes
        .iter()
        .map(|k| {
            let mut cs = k.clone();
            cs.sort_by_key(|c| (first.get(c).copied().unwrap_or(usize::MAX), *c));
            let mut ls: Vec<i64> = k.iter().map(|c| rm0[c]).collect();
            ls.sort_by_key(|a| (geo.get(a).copied().unwrap_or((i32::MAX, i32::MAX)), *a));
            ls.into_iter().zip(cs).collect()
        })
        .collect();
    let with = |extra: &[&Vec<(i64, i64)>]| -> Vec<(i64, i64)> {
        let mut v = seed.to_vec();
        for e in extra {
            v.extend_from_slice(e);
        }
        v
    };
    let mut chosen: Option<Found> = None;
    let all = search(&with(&pref.iter().collect::<Vec<_>>()));
    if all.0 {
        chosen = Some(all);
    } else {
        // classes that depend on each other: add them one at a time
        let mut acc: Vec<&Vec<(i64, i64)>> = vec![];
        for p in &pref {
            acc.push(p);
            let r = search(&with(&acc));
            if r.0 {
                chosen = Some(r);
            } else {
                acc.pop();
            }
        }
    }
    let cm: HashMap<i64, i64> = chosen.as_ref().map(|r| r.3.iter().map(|(&a, &c)| (c, a)).collect()).unwrap_or_else(|| rm0.clone());
    let mut nets = vec![];
    for k in &classes {
        let moved = k.iter().any(|c| cm.get(c) != rm0.get(c));
        let amb = moved || {
            // the chosen pairing rotated by one inside the class, every other net free
            let ls: Vec<i64> = k.iter().map(|c| cm[c]).collect();
            let rot: Vec<(i64, i64)> = (0..k.len()).map(|i| (ls[(i + 1) % k.len()], k[i])).collect();
            search(&with(&[&rot])).0
        };
        if amb {
            nets.extend_from_slice(k);
        }
    }
    let ns: HashSet<i64> = nets.iter().copied().collect();
    let devs: Vec<usize> = cdev_nets.iter().enumerate().filter(|(_, n)| n[..3].iter().any(|c| ns.contains(c))).map(|(i, _)| i).collect();
    (nets, devs, chosen.map(|r| (r.2, r.3, r.4)))
}

struct Split<'a> {
    ex: &'a Extraction,
    mm: &'a HashMap<i64, i64>,
    image: &'a HashSet<i64>,
    cnets: &'a [[i64; 4]],
    csize: &'a [(f64, f64, f64)],
    fs: Vec<(usize, Vec<usize>)>,
    fill: HashMap<usize, (f64, f64)>,
    l2c: HashMap<usize, i64>,
    c2l: HashMap<i64, usize>,
    cur: Vec<usize>,
    /// found splits with the CDL-to-layout binding of their series-internal nets
    sols: Vec<(Vec<usize>, HashMap<i64, usize>)>,
    budget: usize,
    /// only accept a split that binds this CDL net to another layout net than this one
    avoid: Option<(i64, usize)>,
}

impl Split<'_> {
    fn bind(&mut self, a: usize, c: i64, undo: &mut Vec<usize>) -> bool {
        if let Some(&x) = self.mm.get(&(a as i64)) {
            return x == c;
        }
        if let Some(&x) = self.l2c.get(&a) {
            return x == c;
        }
        if self.image.contains(&c) || self.c2l.contains_key(&c) {
            return false;
        }
        self.l2c.insert(a, c);
        self.c2l.insert(c, a);
        undo.push(a);
        true
    }
    fn unbind(&mut self, undo: &[usize]) {
        for a in undo {
            if let Some(c) = self.l2c.remove(a) {
                self.c2l.remove(&c);
            }
        }
    }
    /// fingers in layout order get CDL devices in order, true stops the search
    fn go(&mut self, i: usize, devs: &[usize]) -> bool {
        if self.budget == 0 {
            return true;
        }
        self.budget -= 1;
        if i == self.fs.len() {
            let full = devs.iter().all(|d| {
                let (f, w) = self.fill.get(d).copied().unwrap_or((0.0, 0.0));
                (f - self.csize[*d].0).abs() < 1e-6 && size_eq(w, self.csize[*d].1)
            });
            let other = self.avoid.map_or(true, |(c, a)| self.c2l.get(&c) != Some(&a));
            if full && other {
                self.sols.push((self.cur.clone(), self.c2l.clone()));
            }
            return !self.sols.is_empty();
        }
        let (f, cands) = (self.fs[i].0, self.fs[i].1.clone());
        let ex = self.ex;
        let dv = &ex.devices[f];
        if dv.sd.len() != 2 {
            return false;
        }
        let (ff, fw) = (dv.nfin, if ex.size_wl { dv.w } else { 0.0 });
        for d in cands {
            let (hf, hw) = self.fill.get(&d).copied().unwrap_or((0.0, 0.0));
            let (wf, ww, _) = self.csize[d];
            if hf + ff > wf + 1e-6 || (ww > 0.0 && hw + fw > ww * (1.0 + 5e-4)) {
                continue;
            }
            let [cd, _, cs, _] = self.cnets[d];
            for (x, y) in [(cd, cs), (cs, cd)] {
                let mut undo = vec![];
                if self.bind(dv.sd[0], x, &mut undo) && self.bind(dv.sd[1], y, &mut undo) {
                    self.fill.insert(d, (hf + ff, hw + fw));
                    self.cur.push(d);
                    let stop = self.go(i + 1, devs);
                    self.cur.pop();
                    self.fill.insert(d, (hf, hw));
                    self.unbind(&undo);
                    if stop {
                        return true;
                    }
                } else {
                    self.unbind(&undo);
                }
            }
        }
        false
    }
}

/// leaves that merged parallel copies (several CDL devices on one finger set): give each CDL device its own
/// fingers with consistent source/drain nets, ties by order (fingers left to right, devices in file order).
/// Returns the new leaf pairs and the CDL devices whose fingers could also go another way.
fn split_merged(ex: &Extraction, mm: &HashMap<i64, i64>, cnets: &[[i64; 4]], csize: &[(f64, f64, f64)], lp: &[(Vec<usize>, Vec<usize>)], fxy: &[(i32, i32)]) -> (Vec<(Vec<usize>, Vec<usize>)>, Vec<usize>) {
    let merged: Vec<usize> = (0..lp.len()).filter(|&i| lp[i].1.len() > 1).collect();
    if merged.is_empty() {
        return (lp.to_vec(), vec![]);
    }
    // merged leaves sharing an unbound (series-internal) layout net are solved together
    fn root(p: &mut [usize], mut a: usize) -> usize {
        while p[a] != a {
            p[a] = p[p[a]];
            a = p[a];
        }
        a
    }
    let mut parent: Vec<usize> = (0..merged.len()).collect();
    let mut owner: HashMap<usize, usize> = HashMap::new();
    for (k, &i) in merged.iter().enumerate() {
        for &f in &lp[i].0 {
            for &a in &ex.devices[f].sd {
                if mm.contains_key(&(a as i64)) {
                    continue;
                }
                match owner.get(&a) {
                    Some(&o) => {
                        let (x, y) = (root(&mut parent, o), root(&mut parent, k));
                        parent[x] = y;
                    }
                    None => {
                        owner.insert(a, k);
                    }
                }
            }
        }
    }
    let mut comps: Vec<Vec<usize>> = vec![];
    let mut at: HashMap<usize, usize> = HashMap::new();
    for k in 0..merged.len() {
        let r = root(&mut parent, k);
        let e = *at.entry(r).or_insert_with(|| {
            comps.push(vec![]);
            comps.len() - 1
        });
        comps[e].push(k);
    }
    let image: HashSet<i64> = mm.values().copied().collect();
    // CDL devices are tried in order of first use of their unbound source/drain net, as nets are paired above
    let mut first: HashMap<i64, usize> = HashMap::new();
    for (i, n) in cnets.iter().enumerate() {
        for &c in &n[..3] {
            first.entry(c).or_insert(i);
        }
    }
    let rank = |d: usize| -> (usize, usize) { ([cnets[d][0], cnets[d][2]].iter().filter(|c| !image.contains(c)).map(|c| first[c]).min().unwrap_or(usize::MAX), d) };
    let mut out: Vec<(Vec<usize>, Vec<usize>)> = lp.iter().filter(|p| p.1.len() <= 1).cloned().collect();
    let mut amb = vec![];
    for comp in comps {
        let mut fs: Vec<(usize, Vec<usize>)> = vec![];
        let mut devs: Vec<usize> = vec![];
        for &k in &comp {
            let (lm, cm) = &lp[merged[k]];
            let mut c = cm.clone();
            c.sort_by_key(|&d| rank(d));
            devs.extend_from_slice(&c);
            for &f in lm {
                fs.push((f, c.clone()));
            }
        }
        fs.sort_by_key(|(f, _)| (fxy[*f], *f));
        let run = |avoid: Option<(i64, usize)>| -> Option<(Vec<usize>, HashMap<i64, usize>)> {
            let mut st = Split { ex, mm, image: &image, cnets, csize, fs: fs.clone(), fill: HashMap::new(), l2c: HashMap::new(), c2l: HashMap::new(), cur: vec![], sols: vec![], budget: 200_000, avoid };
            st.go(0, &devs);
            st.sols.pop()
        };
        let (sol, c2l) = match run(None) {
            Some(x) => x,
            None => {
                // no consistent split (or the budget ran out): the devices keep sharing their fingers
                for &k in &comp {
                    out.push(lp[merged[k]].clone());
                }
                continue;
            }
        };
        // a series-internal net is ambiguous when another split binds it to another layout net;
        // parallel duplicates (no internal net) swap without changing any net and are not marked
        let mut moved: HashSet<i64> = HashSet::new();
        let mut nets: Vec<(i64, usize)> = c2l.iter().map(|(&c, &a)| (c, a)).collect();
        nets.sort();
        for (c, a) in nets {
            if !moved.contains(&c) {
                if let Some((_, c2)) = run(Some((c, a))) {
                    moved.extend(c2.iter().filter(|(x, y)| c2l.get(x) != Some(y)).map(|(x, _)| *x));
                }
            }
        }
        for &d in &devs {
            let fl: Vec<usize> = fs.iter().zip(&sol).filter(|(_, &x)| x == d).map(|((f, _), _)| *f).collect();
            if moved.contains(&cnets[d][0]) || moved.contains(&cnets[d][2]) {
                amb.push(d);
            }
            out.push((fl, vec![d]));
        }
    }
    (out, amb)
}

/// plain-word checks of the pin texts against the CDL ports, and CDL lines the reader had to drop
pub fn pin_causes(ex: &Extraction, sc: &Subckt) -> Vec<String> {
    let mut out: Vec<String> = sc.dropped.iter().map(|d| crate::trf!("the CDL lost a transistor to a malformed line: {}", d)).collect();
    if sc.mos.is_empty() {
        out.push(crate::i18n::tr("the CDL subcircuit has no transistors the reader accepted").into());
    }
    let ports: HashSet<&str> = sc.ports.iter().map(|s| s.as_str()).collect();
    let cdl_nets: HashSet<&str> = ports.iter().copied().chain(sc.mos.iter().flat_map(|d| [d.d.as_str(), d.g.as_str(), d.s.as_str(), d.b.as_str()])).collect();
    let on_gate: HashSet<&str> = sc.mos.iter().map(|d| d.g.as_str()).collect();
    let on_sd: HashSet<&str> = sc.mos.iter().flat_map(|d| [d.d.as_str(), d.s.as_str()]).collect();
    let mut texts: Vec<(&str, usize)> = ex.nets.iter().flat_map(|n| n.names.iter().map(move |t| (t.as_str(), n.id))).collect();
    texts.sort();
    texts.dedup();
    let mut stray: Vec<&str> = vec![];
    for &(t, id) in &texts {
        let g = ex.devices.iter().any(|d| d.gate == id as i64);
        let s = ex.devices.iter().any(|d| d.sd.contains(&id));
        let b = ex.devices.iter().any(|d| d.bulk == id as i64);
        if !cdl_nets.contains(t) {
            stray.push(t);
            match sc.ports.iter().find(|p| p.eq_ignore_ascii_case(t)) {
                Some(p) => out.push(crate::trf!("pin text {} has no CDL port of that name (the CDL port is {}, names are compared exactly)", t, p)),
                None => out.push(crate::trf!("pin text {} has no CDL port of that name", t)),
            }
        }
        if !g && !s && !b {
            let layers = ex.nets.get(id).map(|n| n.layers.join(", ")).unwrap_or_default();
            out.push(crate::trf!("pin text {} is on a net with no devices (its shapes are only on {}, nothing connects them down to a gate or diffusion)", t, layers));
        } else if ports.contains(t) {
            if on_gate.contains(t) && !on_sd.contains(t) && s && !g {
                out.push(crate::trf!("pin text {} is on a diffusion (source/drain) net, but CDL port {} connects only to gates", t, t));
            }
            if on_sd.contains(t) && !on_gate.contains(t) && g && !s {
                out.push(crate::trf!("pin text {} is on a gate net, but CDL port {} connects only to source/drain", t, t));
            }
        }
    }
    let named: HashSet<&str> = texts.iter().map(|x| x.0).collect();
    let bare: Vec<&str> = sc.ports.iter().map(|s| s.as_str()).filter(|p| (on_gate.contains(p) || on_sd.contains(p)) && !named.contains(p)).collect();
    for p in &bare {
        out.push(crate::trf!("CDL port {} has no pin text in the layout", p));
    }
    if stray.len() == 1 && bare.len() == 1 && !stray[0].eq_ignore_ascii_case(bare[0]) {
        out.push(crate::trf!("pin text {} and CDL port {} are the only unpaired names: the same pin may have two names", stray[0], bare[0]));
    }
    out
}
