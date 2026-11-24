//! Logic check of a cell: every input vector is simulated on the transistors at switch level and each output
//! is compared with a Boolean function written as in Liberty (`Y = !(A & B)`, one output per line).
//! An nmos conducts with its gate at 1, a pmos with its gate at 0, either may conduct with its gate at X or Z.
//! A node is 1 (0) when a conducting path reaches that level and no path that may conduct reaches the other,
//! Z when no path that may conduct reaches a supply or an input, and X otherwise (a fight or an undecided gate).

use crate::cdl::Subckt;
use crate::devmatch::MatchResult;
use crate::extract::Extraction;
use crate::i18n::tr;
use crate::liberty::LCell;
use crate::logic::{expr_vars, parse_bool, Expr};
use crate::trf;
use std::collections::HashMap;

/// most inputs checked (2^16 vectors)
pub const MAX_INPUTS: usize = 16;
pub const X: u8 = 2;
pub const Z: u8 = 3;

/// the level of input k of n in vector v: the first input is the highest bit, so a truth table in vector
/// order counts up with the last input changing fastest
pub fn input_bit(v: usize, k: usize, n: usize) -> u8 {
    ((v >> (n - 1 - k)) & 1) as u8
}

pub fn level_char(v: u8) -> char {
    match v {
        0 => '0',
        1 => '1',
        X => 'X',
        _ => 'Z',
    }
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub out: String,
    pub f: Expr,
    /// the output is high impedance while this is 1 (Liberty three_state)
    pub z: Option<Expr>,
}

fn is_name(s: &str) -> bool {
    s.chars().next().map_or(false, |c| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '[' | ']' | '.' | '$'))
}

/// `OUT = FUNCTION` and `three_state(OUT) = FUNCTION`, one per line or separated by ';', '#' starts a comment
pub fn parse_spec(text: &str) -> Result<Vec<Spec>, String> {
    let mut out: Vec<Spec> = vec![];
    let mut zs: Vec<(String, Expr)> = vec![];
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        for part in line.split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (l, r) = part.split_once('=').ok_or_else(|| trf!("'{}' has no = (write OUTPUT = function)", part))?;
            let (l, r) = (l.trim(), r.trim().trim_matches('"').trim());
            let e = parse_bool(r)?;
            if let Some(rest) = l.strip_prefix("three_state").or_else(|| l.strip_prefix("THREE_STATE")).filter(|r| r.trim_start().starts_with('(')) {
                let n = rest.trim();
                let n = n.strip_prefix('(').and_then(|n| n.strip_suffix(')')).unwrap_or(n).trim();
                if !is_name(n) {
                    return Err(trf!("write three_state(OUTPUT) = function, not '{}'", l));
                }
                zs.push((n.to_string(), e));
            } else {
                if !is_name(l) {
                    return Err(trf!("'{}' is not a pin name", l));
                }
                if out.iter().any(|s| s.out == l) {
                    return Err(trf!("{} is given twice", l));
                }
                out.push(Spec { out: l.to_string(), f: e, z: None });
            }
        }
    }
    for (n, e) in zs {
        match out.iter_mut().find(|s| s.out == n) {
            Some(s) => s.z = Some(e),
            None => return Err(trf!("three_state({}) names no output with a function", n)),
        }
    }
    if out.is_empty() {
        return Err(tr("no function given").into());
    }
    Ok(out)
}

/// the output functions of a Liberty cell in the form parse_spec reads, or why there is nothing to check
pub fn liberty_text(c: &LCell) -> Result<String, String> {
    if c.ff.is_some() || c.latch.is_some() || c.statetable || c.icg || c.bank {
        return Err(tr("sequential cell (flip-flop or latch), only combinational outputs are checked").into());
    }
    let mut lines = vec![];
    for p in c.pins.iter().filter(|p| p.dir == "output" || p.dir == "inout") {
        if let Some(f) = p.func.as_ref().map(|f| f.trim()).filter(|f| !f.is_empty()) {
            lines.push(format!("{} = {}", p.name, f));
            if let Some(t) = p.three_state.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
                lines.push(format!("three_state({}) = {}", p.name, t));
            }
        }
    }
    if lines.is_empty() {
        return Err(tr("no output with a function in the Liberty file").into());
    }
    // a function or three_state naming an output (a bus holder: Z = Z) keeps a state
    let outs: Vec<&str> = c.pins.iter().filter(|p| p.func.is_some() && (p.dir == "output" || p.dir == "inout")).map(|p| p.name.as_str()).collect();
    for p in c.pins.iter().filter(|p| outs.contains(&p.name.as_str())) {
        let mut vars = vec![];
        for f in [p.func.as_deref(), p.three_state.as_deref()].into_iter().flatten() {
            if let Ok(e) = parse_bool(f) {
                expr_vars(&e, &mut vars);
            }
        }
        if vars.iter().any(|v| outs.iter().any(|o| o.eq_ignore_ascii_case(v))) {
            return Err(tr("holds a state (an output function names an output), only combinational outputs are checked").into());
        }
    }
    Ok(lines.join("\n"))
}

/// supply level of a net by its name: VDD, VCC, VPWR, VPB, VNW ... are 1, VSS, GND, VGND, VNB, VPW ... are 0
pub fn rail_level(name: &str) -> Option<u8> {
    let n = name.trim_end_matches('!').to_ascii_lowercase();
    if n.starts_with("vdd") || n.starts_with("vcc") || n.starts_with("vpwr") || matches!(n.as_str(), "vpb" | "vnw" | "vbp" | "kapwr" | "lvpwr" | "lowlvpwr" | "vpp") {
        Some(1)
    } else if n.starts_with("vss") || n.starts_with("gnd") || n.starts_with("vgnd") || matches!(n.as_str(), "vnb" | "vpw" | "vbn" | "vbb" | "0") {
        Some(0)
    } else {
        None
    }
}

fn root(u: &mut [usize], mut i: usize) -> usize {
    while u[i] != i {
        u[i] = u[u[i]];
        i = u[i];
    }
    i
}

fn union(u: &mut [usize], a: usize, b: usize) {
    let (ra, rb) = (root(u, a), root(u, b));
    if ra != rb {
        u[rb] = ra;
    }
}

/// most nodes left undecided by the three-valued relaxation whose steady states are searched (2^16 assignments)
pub const MAX_STATE_NODES: usize = 16;
/// state evaluations one check of a circuit may spend on that search over all its vectors (about a second),
/// so the viewer never stalls on a circuit that keeps many nodes undecided; past it the nodes stay X
pub const SEARCH_BUDGET: u64 = 1 << 22;

/// 2 conducts, 0 is off, 1 may conduct (gate X or Z)
fn state(pm: bool, g: u8) -> u8 {
    match (pm, g) {
        (false, 1) | (true, 0) => 2,
        (false, 0) | (true, 1) => 0,
        _ => 1,
    }
}

/// conductance of a transistor for breaking a fight: W/L (fins when given), times m, a pmos at half an nmos
fn strength(pmos: bool, w: f64, l: f64, nfin: f64, m: f64) -> f64 {
    let ok = |x: f64| x.is_finite() && x > 0.0;
    let width = if ok(nfin) { nfin } else if ok(w) { w } else { 1.0 };
    let len = if ok(l) { l } else { 1.0 };
    width / len * if ok(m) { m } else { 1.0 } * if pmos { 0.5 } else { 1.0 }
}

/// union-find buffers reused by every evaluation
struct Scratch {
    on: Vec<usize>,
    may: Vec<usize>,
    flag: Vec<u8>,
}

impl Scratch {
    fn new(n: usize) -> Scratch {
        Scratch { on: vec![0; n], may: vec![0; n], flag: vec![0; n] }
    }
}

/// transistors as switches between nodes
#[derive(Default, Clone)]
pub struct Circuit {
    /// the names of each node (a layout net can carry several texts)
    pub names: Vec<Vec<String>>,
    /// (pmos, gate, one end, the other end)
    sw: Vec<(bool, usize, usize, usize)>,
    /// conductance of each transistor for breaking a fight (see strength)
    size: Vec<f64>,
    /// supply nodes and their level
    pub rails: Vec<(usize, u8)>,
    /// the pins: subcircuit ports, or the texts of the layout
    pub ports: Vec<String>,
    ix: HashMap<String, usize>,
    /// nodes named as a supply at 1 and at 0
    shorted: Vec<usize>,
}

impl Circuit {
    fn node(&mut self, name: &str) -> usize {
        if let Some(&i) = self.ix.get(name) {
            return i;
        }
        self.names.push(vec![name.to_string()]);
        self.ix.insert(name.to_string(), self.names.len() - 1);
        self.names.len() - 1
    }

    fn alias(&mut self, i: usize, name: &str) {
        if !self.ix.contains_key(name) {
            self.ix.insert(name.to_string(), i);
            self.names[i].push(name.to_string());
        }
    }

    fn find_rails(&mut self, vdd: Option<&str>, vss: Option<&str>) {
        for (i, ns) in self.names.iter().enumerate() {
            let lv: Vec<u8> = ns.iter().filter_map(|n| rail_level(n)).collect();
            if let Some(&v) = lv.first() {
                self.rails.push((i, v));
                if lv.iter().any(|&x| x != v) {
                    // a net named as a supply at 1 and at 0: the supplies are shorted
                    self.shorted.push(i);
                }
            }
        }
        // no supply named as one: the nets the transistor bodies tie to
        for (want, n) in [(1u8, vdd), (0u8, vss)] {
            if !self.rails.iter().any(|r| r.1 == want) {
                if let Some(&i) = n.and_then(|n| self.ix.get(n)) {
                    if !self.rails.iter().any(|r| r.0 == i) {
                        self.rails.push((i, want));
                    }
                }
            }
        }
    }

    pub fn transistors(&self) -> usize {
        self.sw.len()
    }

    /// the transistors of a CDL or post-layout subcircuit (zero-ohm shorts join their nets)
    pub fn from_subckt(sc: &Subckt) -> Circuit {
        let mut uf: HashMap<String, String> = HashMap::new();
        let top = |uf: &HashMap<String, String>, s: &str| {
            let mut s = s.to_string();
            while let Some(n) = uf.get(&s) {
                s = n.clone();
            }
            s
        };
        for (a, b) in &sc.shorts {
            let (ra, rb) = (top(&uf, a), top(&uf, b));
            if ra != rb {
                uf.insert(rb, ra);
            }
        }
        let mut c = Circuit::default();
        let get = |c: &mut Circuit, n: &str| {
            let i = c.node(&top(&uf, n));
            c.alias(i, n);
            i
        };
        for p in &sc.ports {
            get(&mut c, p);
        }
        c.ports = sc.ports.clone();
        for (a, b) in &sc.shorts {
            get(&mut c, a);
            get(&mut c, b);
        }
        for d in &sc.mos {
            let p = match d.typ.as_str() {
                "pmos" => true,
                "nmos" => false,
                _ => continue,
            };
            let (g, a, b) = (get(&mut c, &d.g), get(&mut c, &d.d), get(&mut c, &d.s));
            c.sw.push((p, g, a, b));
            c.size.push(strength(p, d.w, d.l, d.nfin, d.m));
        }
        let s = crate::xtor::supplies(sc);
        let (vdd, vss) = (s.vdd.map(|n| top(&uf, &n)), s.vss.map(|n| top(&uf, &n)));
        c.find_rails(vdd.as_deref(), vss.as_deref());
        c
    }

    /// the transistors extracted from the layout: nets by their texts (nets with one text are one node),
    /// a net without a text by the CDL net the LVS match paired it with
    pub fn from_extraction(ex: &Extraction, m: Option<&MatchResult>) -> Circuit {
        let mut c = Circuit::default();
        let texts: Vec<Vec<String>> = ex
            .nets
            .iter()
            .map(|nt| {
                let mut t: Vec<String> = nt.name.iter().cloned().collect();
                for n in &nt.names {
                    if !t.contains(n) {
                        t.push(n.clone());
                    }
                }
                t
            })
            .collect();
        // nets that share a text are one node, transitively whatever the net order ([A] [B] [A,B] is one node)
        let mut u: Vec<usize> = (0..texts.len()).collect();
        let mut first: HashMap<&str, usize> = HashMap::new();
        for (i, ts) in texts.iter().enumerate() {
            for t in ts {
                match first.get(t.as_str()) {
                    Some(&j) => union(&mut u, j, i),
                    None => {
                        first.insert(t.as_str(), i);
                    }
                }
            }
        }
        let mut node_of: HashMap<usize, usize> = HashMap::new();
        let mut of: Vec<usize> = Vec::with_capacity(texts.len());
        for (i, ts) in texts.iter().enumerate() {
            let r = root(&mut u, i);
            let k = match node_of.get(&r) {
                Some(&k) => k,
                None => {
                    let k = c.node(ts.first().cloned().unwrap_or_else(|| format!("#{}", i)).as_str());
                    node_of.insert(r, k);
                    k
                }
            };
            for t in ts {
                c.alias(k, t);
                if !c.ports.contains(t) {
                    c.ports.push(t.clone());
                }
            }
            c.alias(k, &format!("#{}", i));
            of.push(k);
        }
        for (i, &k) in of.iter().enumerate() {
            if let Some(n) = m.and_then(|m| m.net_map.get(&i)) {
                c.alias(k, n);
            }
        }
        let mut floating = 0usize;
        for d in &ex.devices {
            let p = match d.typ.as_str() {
                "pmos" => true,
                "nmos" => false,
                _ => continue,
            };
            if d.sd.len() < 2 {
                continue;
            }
            let g = if d.gate >= 0 && (d.gate as usize) < of.len() {
                of[d.gate as usize]
            } else {
                floating += 1;
                c.node(&format!("#gate{}", floating))
            };
            c.sw.push((p, g, of[d.sd[0]], of[d.sd[1]]));
            c.size.push(strength(p, d.w, d.l, d.nfin, 1.0));
        }
        c.find_rails(None, None);
        c
    }

    /// the node of a pin name: exact, else the one node whose name matches ignoring case
    pub fn find(&self, name: &str) -> Option<usize> {
        if let Some(&i) = self.ix.get(name) {
            return Some(i);
        }
        let mut hit = self.ix.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, &i)| i);
        let first = hit.next()?;
        if hit.all(|i| i == first) {
            Some(first)
        } else {
            None
        }
    }

    /// levels of all nodes with the given nodes held: the levels every steady state of the circuit agrees on.
    /// A three-valued relaxation from all X comes first (an undecided gate or a fight gives X, a node that reaches
    /// nothing Z); its 0 and 1 hold in every steady state. The nodes it leaves X (pass gates that hold each
    /// other's gates, a cross-coupled level shifter, the diode node of a tie cell, or real feedback) are then
    /// tried at every 0/1 assignment that gives each node only a level some path could put on it: an assignment is
    /// a steady state when one evaluation gives it back, a node that reaches nothing keeping its charge (so a
    /// state held by charge after an input change counts). Such a node takes a level only when every steady state has it, so
    /// a circuit that can hold two states, a fight, or more than MAX_STATE_NODES undecided nodes stays X.
    /// A transistor that may conduct between two held nodes at different levels (a short of a supply or an input)
    /// makes every node X.
    pub fn settle(&self, held: &[(usize, u8)]) -> Vec<u8> {
        let mut budget = SEARCH_BUDGET;
        self.settle_within(held, &mut budget).0
    }

    /// settle, spending at most `budget` state evaluations on the search (left X once it runs out). Returns the
    /// levels and the levels gates see (a floating node at the charge it keeps rather than Z)
    fn settle_within(&self, held: &[(usize, u8)], budget: &mut u64) -> (Vec<u8>, Vec<u8>) {
        let n = self.names.len();
        let mut fixed = vec![false; n];
        let mut val = vec![X; n];
        for &(i, v) in held {
            val[i] = v;
            fixed[i] = true;
        }
        let mut sc = Scratch::new(n);
        val = self.relax(val, &fixed, &mut sc);
        let mut gates = val.clone();
        let open: Vec<usize> = (0..n).filter(|&i| val[i] == X).collect();
        if !open.is_empty() && open.len() <= MAX_STATE_NODES && (1u64 << open.len()) <= *budget {
            *budget -= 1u64 << open.len();
            let (can1, can0) = self.drivable(&fixed);
            let adj = self.adjacency();
            let mut agree: Option<Vec<u8>> = None;
            // a steady state without a fight exists (states held only by a fight are a static short)
            let mut clean = false;
            let mut s = val.clone();
            for m in 0u32..(1u32 << open.len()) {
                for (k, &i) in open.iter().enumerate() {
                    s[i] = ((m >> k) & 1) as u8;
                }
                // a level no path could ever have put on the node is not a state it can hold
                if open.iter().any(|&i| if s[i] == 1 { !can1[i] } else { !can0[i] }) {
                    continue;
                }
                let next = self.step(&s, &fixed, true, &mut sc);
                // a node in a fight keeps the level the stronger side gives it (a keeper, a ratioed latch)
                let mut fight = false;
                let steady = open.iter().all(|&i| {
                    next[i] == s[i]
                        || (next[i] == X && self.fight_level(&s, &fixed, i, &adj) == s[i] && {
                            fight = true;
                            true
                        })
                });
                if steady {
                    clean |= !fight;
                    agree = Some(match agree {
                        None => open.iter().map(|&i| s[i]).collect(),
                        Some(a) => a.iter().zip(&open).map(|(&x, &i)| if x == s[i] { x } else { X }).collect(),
                    });
                }
            }
            if !clean {
                agree = None;
            }
            if let Some(a) = agree {
                for (k, &i) in open.iter().enumerate() {
                    s[i] = a[k];
                }
                // a node floating in that state holds its level only as charge on gates: it reads Z, so an
                // output nothing drives never passes as a level
                let driven = self.step(&s, &fixed, false, &mut sc);
                for (k, &i) in open.iter().enumerate() {
                    gates[i] = a[k];
                    val[i] = if a[k] <= 1 && driven[i] == Z { Z } else { a[k] };
                }
            }
        }
        if self.sw.iter().any(|&(pm, g, a, b)| fixed[a] && fixed[b] && gates[a] != gates[b] && state(pm, gates[g]) != 0) {
            for i in 0..n {
                if !fixed[i] {
                    val[i] = X;
                    gates[i] = X;
                }
            }
        }
        (val, gates)
    }

    /// the levels each node could ever be driven to: 1 (0) when channels, whatever their gates, connect it to a
    /// supply at 1 (0) or to an input (which takes both levels)
    fn drivable(&self, fixed: &[bool]) -> (Vec<bool>, Vec<bool>) {
        let n = self.names.len();
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
        for &(_, _, a, b) in &self.sw {
            adj[a].push(b);
            adj[b].push(a);
        }
        let rail = |i: usize| self.rails.iter().find(|r| r.0 == i).map(|r| r.1);
        let reach = |lvl: u8| {
            let mut seen = vec![false; n];
            let mut q: Vec<usize> = (0..n).filter(|&i| fixed[i] && rail(i).map_or(true, |r| r == lvl)).collect();
            while let Some(y) = q.pop() {
                for &x in &adj[y] {
                    if !fixed[x] && !seen[x] {
                        seen[x] = true;
                        q.push(x);
                    }
                }
            }
            seen
        };
        (reach(1), reach(0))
    }

    /// the transistors touching each node
    fn adjacency(&self) -> Vec<Vec<usize>> {
        let mut adj: Vec<Vec<usize>> = vec![vec![]; self.names.len()];
        for (k, &(_, _, a, b)) in self.sw.iter().enumerate() {
            adj[a].push(k);
            if b != a {
                adj[b].push(k);
            }
        }
        adj
    }

    /// the level a fight on node i settles to by the transistor sizes: the conducting channels around it are a
    /// resistor network between held nodes, solved for its voltage (1 above 0.55 of the supply, 0 below 0.45).
    /// X in between, or when a channel that may conduct (gate X or Z) touches the network
    fn fight_level(&self, val: &[u8], fixed: &[bool], i: usize, adj: &[Vec<usize>]) -> u8 {
        let mut comp = vec![i];
        let mut idx: HashMap<usize, usize> = HashMap::from([(i, 0)]);
        let mut k = 0;
        while k < comp.len() {
            let x = comp[k];
            k += 1;
            for &e in &adj[x] {
                let (pm, g, a, b) = self.sw[e];
                match state(pm, val[g]) {
                    0 => continue,
                    1 => return X,
                    _ => {}
                }
                let y = if a == x { b } else { a };
                if !fixed[y] && !idx.contains_key(&y) {
                    idx.insert(y, comp.len());
                    comp.push(y);
                }
            }
        }
        let m = comp.len();
        if m > 64 {
            return X;
        }
        let mut g = vec![vec![0.0f64; m + 1]; m];
        for (r, &x) in comp.iter().enumerate() {
            for &e in &adj[x] {
                let (pm, gate, a, b) = self.sw[e];
                let y = if a == x { b } else { a };
                if state(pm, val[gate]) != 2 || y == x {
                    continue;
                }
                let c = self.size[e];
                g[r][r] += c;
                if fixed[y] {
                    g[r][m] += c * val[y] as f64;
                } else {
                    g[r][idx[&y]] -= c;
                }
            }
        }
        // Gaussian elimination with partial pivoting
        for col in 0..m {
            let piv = (col..m).max_by(|&a, &b| g[a][col].abs().total_cmp(&g[b][col].abs())).unwrap_or(col);
            if g[piv][col].abs() < 1e-12 {
                return X;
            }
            g.swap(col, piv);
            for r in 0..m {
                if r != col {
                    let f = g[r][col] / g[col][col];
                    if f != 0.0 {
                        for c in col..=m {
                            g[r][c] -= f * g[col][c];
                        }
                    }
                }
            }
        }
        let v = g[0][m] / g[0][0];
        if v > 0.55 {
            1
        } else if v < 0.45 {
            0
        } else {
            X
        }
    }

    /// node o at level lvl is driven at full swing: a conducting path of pmos (for 1) or nmos (for 0) reaches a
    /// held node at that level; a 1 that only comes through an nmos (or a 0 through a pmos) is a degraded level
    fn full_swing(&self, val: &[u8], fixed: &[bool], o: usize, lvl: u8, adj: &[Vec<usize>]) -> bool {
        if fixed[o] {
            return true;
        }
        let mut seen = vec![false; val.len()];
        seen[o] = true;
        let mut q = vec![o];
        while let Some(x) = q.pop() {
            for &e in &adj[x] {
                let (pm, g, a, b) = self.sw[e];
                if pm != (lvl == 1) || state(pm, val[g]) != 2 {
                    continue;
                }
                let y = if a == x { b } else { a };
                if fixed[y] {
                    if val[y] == lvl {
                        return true;
                    }
                } else if !seen[y] {
                    seen[y] = true;
                    q.push(y);
                }
            }
        }
        false
    }

    /// one evaluation of every node that is not held, from the levels `val`: a node is 1 (0) when a conducting
    /// path reaches that level and no path that may conduct reaches the other, X when both may be reached, and
    /// when no path that may conduct reaches a held node it is Z, or keeps its level with `keep` (charge)
    fn step(&self, val: &[u8], fixed: &[bool], keep: bool, sc: &mut Scratch) -> Vec<u8> {
        let n = val.len();
        let (on, may, flag) = (&mut sc.on, &mut sc.may, &mut sc.flag);
        for i in 0..n {
            on[i] = i;
            may[i] = i;
            flag[i] = 0;
        }
        for &(pm, g, a, b) in &self.sw {
            let s = state(pm, val[g]);
            if s == 0 || fixed[a] || fixed[b] {
                continue;
            }
            if s == 2 {
                union(on, a, b);
            }
            union(may, a, b);
        }
        // bits 1/2: a conducting path reaches 1/0, bits 4/8: a path that may conduct reaches 1/0
        for &(pm, g, a, b) in &self.sw {
            let s = state(pm, val[g]);
            if s == 0 {
                continue;
            }
            for (x, y) in [(a, b), (b, a)] {
                if !fixed[x] && fixed[y] {
                    let hi = val[y] == 1;
                    if s == 2 {
                        let r = root(on, x);
                        flag[r] |= if hi { 1 } else { 2 };
                    }
                    let r = root(may, x);
                    flag[r] |= if hi { 4 } else { 8 };
                }
            }
        }
        let mut next = val.to_vec();
        for i in 0..n {
            if fixed[i] {
                continue;
            }
            let fd = flag[root(on, i)] & 3;
            let fp = flag[root(may, i)] & 12;
            next[i] = if fd & 1 != 0 && fp & 8 == 0 {
                1
            } else if fd & 2 != 0 && fp & 4 == 0 {
                0
            } else if fp == 0 {
                if keep {
                    val[i]
                } else {
                    Z
                }
            } else {
                X
            };
        }
        next
    }

    /// three-valued relaxation: evaluations repeated until nothing changes; in a ring that never settles the
    /// nodes still changing are X
    fn relax(&self, mut val: Vec<u8>, fixed: &[bool], sc: &mut Scratch) -> Vec<u8> {
        let limit = 2 * val.len() + 8;
        for it in 0..=limit {
            let mut next = self.step(&val, fixed, false, sc);
            if next == val {
                return val;
            }
            if it == limit {
                for i in 0..val.len() {
                    if next[i] != val[i] {
                        next[i] = X;
                    }
                }
            }
            val = next;
        }
        val
    }

    /// output levels for every input vector (see input_bit)
    pub fn simulate(&self, inputs: &[String], outputs: &[String]) -> Result<Vec<Vec<u8>>, String> {
        let outs: Vec<usize> = outputs.iter().map(|n| self.find(n).ok_or_else(|| trf!("{} is not a net of this circuit", n))).collect::<Result<_, _>>()?;
        let mut held = self.holds(inputs)?;
        let base = self.rails.len();
        let mut budget = SEARCH_BUDGET;
        let adj = self.adjacency();
        let mut fixed = vec![false; self.names.len()];
        for &(i, _) in &held {
            fixed[i] = true;
        }
        Ok((0..1usize << inputs.len())
            .map(|v| {
                for k in 0..inputs.len() {
                    held[base + k].1 = input_bit(v, k, inputs.len());
                }
                let (val, gates) = self.settle_within(&held, &mut budget);
                // an output at a degraded level (a 1 only through nmos, a 0 only through pmos) is not a level
                outs.iter().map(|&o| if val[o] <= 1 && !self.full_swing(&gates, &fixed, o, val[o], &adj) { X } else { val[o] }).collect()
            })
            .collect())
    }

    /// levels of every node for one input vector
    pub fn levels(&self, inputs: &[String], v: usize) -> Result<Vec<u8>, String> {
        let mut held = self.holds(inputs)?;
        let base = self.rails.len();
        for k in 0..inputs.len() {
            held[base + k].1 = input_bit(v, k, inputs.len());
        }
        Ok(self.settle(&held))
    }

    /// for tracing one input vector: the levels of the three-valued relaxation and the final levels (steady states)
    pub fn phases(&self, inputs: &[String], v: usize) -> Result<Vec<(&'static str, Vec<u8>)>, String> {
        let mut held = self.holds(inputs)?;
        let base = self.rails.len();
        for k in 0..inputs.len() {
            held[base + k].1 = input_bit(v, k, inputs.len());
        }
        let n = self.names.len();
        let mut fixed = vec![false; n];
        let mut val = vec![X; n];
        for &(i, x) in &held {
            val[i] = x;
            fixed[i] = true;
        }
        let three = self.relax(val, &fixed, &mut Scratch::new(n));
        Ok(vec![("three-valued", three), ("steady states", self.settle(&held))])
    }

    /// the supply nodes, then the input nodes (at 0)
    fn holds(&self, inputs: &[String]) -> Result<Vec<(usize, u8)>, String> {
        let ins: Vec<usize> = inputs.iter().map(|n| self.find(n).ok_or_else(|| trf!("{} is not a net of this circuit", n))).collect::<Result<_, _>>()?;
        for (k, &i) in ins.iter().enumerate() {
            if let Some(r) = self.rails.iter().find(|r| r.0 == i) {
                return Err(trf!("input {} is the supply net {}", inputs[k], self.names[r.0][0]));
            }
            if let Some(j) = ins[..k].iter().position(|&x| x == i) {
                return Err(trf!("inputs {} and {} are one net", inputs[j], inputs[k]));
            }
        }
        if let Some(&i) = self.shorted.first() {
            return Err(trf!("the supplies {} are one net (a short)", self.names[i].iter().filter(|n| rail_level(n).is_some()).cloned().collect::<Vec<_>>().join(" ")));
        }
        if !self.rails.iter().any(|r| r.1 == 1) || !self.rails.iter().any(|r| r.1 == 0) {
            return Err(tr("no supply nets found (named VDD and VSS, VPWR and VGND or the like)").into());
        }
        let mut held = self.rails.clone();
        held.extend(ins.iter().map(|&i| (i, 0u8)));
        Ok(held)
    }
}

fn eval(e: &Expr, ins: &[String], v: usize) -> u8 {
    match e {
        Expr::Var(n) => ins.iter().position(|x| x == n).map_or(X, |k| input_bit(v, k, ins.len())),
        Expr::Const(c) => *c,
        Expr::Not(a) => eval(a, ins, v) ^ 1,
        Expr::And(a) => a.iter().all(|x| eval(x, ins, v) == 1) as u8,
        Expr::Or(a) => a.iter().any(|x| eval(x, ins, v) == 1) as u8,
        Expr::Xor(a) => a.iter().fold(0, |s, x| s ^ eval(x, ins, v)),
    }
}

/// the functions, and what each circuit gives, for every input vector
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    /// wanted level of each output per vector (see input_bit)
    pub want: Vec<Vec<u8>>,
    /// per circuit: its label and the simulated levels, or why it could not be simulated
    pub got: Vec<(String, Result<Vec<Vec<u8>>, String>)>,
    /// pins of the first circuit that are not supplies and not in the functions, left floating
    pub unused: Vec<String>,
}

impl Outcome {
    /// vectors where circuit k differs from the functions, None when it could not be simulated
    pub fn differ(&self, k: usize) -> Option<usize> {
        let g = self.got.get(k)?.1.as_ref().ok()?;
        Some(g.iter().zip(&self.want).filter(|(a, b)| a != b).count())
    }
    pub fn vectors(&self) -> usize {
        self.want.len()
    }
}

/// check the circuits against the functions written in `text`
pub fn run(text: &str, circuits: &[(String, &Circuit)]) -> Result<Outcome, String> {
    let specs = parse_spec(text)?;
    let mut inputs: Vec<String> = vec![];
    for s in &specs {
        expr_vars(&s.f, &mut inputs);
        if let Some(z) = &s.z {
            expr_vars(z, &mut inputs);
        }
    }
    let outputs: Vec<String> = specs.iter().map(|s| s.out.clone()).collect();
    if let Some(o) = outputs.iter().find(|o| inputs.iter().any(|i| i.eq_ignore_ascii_case(o))) {
        return Err(trf!("{} is an output and an input of the functions (a stored state), only combinational functions are checked", o));
    }
    if inputs.len() > MAX_INPUTS {
        return Err(trf!("{} inputs, at most {} are checked", inputs.len(), MAX_INPUTS));
    }
    // inputs in the pin order of the first circuit (A1 A2 B1 B2), names it does not have last
    if let Some((_, c)) = circuits.first() {
        let at = |n: &String| c.ports.iter().position(|p| p.eq_ignore_ascii_case(n)).unwrap_or(usize::MAX);
        inputs.sort_by_key(|n| at(n));
    }
    let want = (0..1usize << inputs.len())
        .map(|v| specs.iter().map(|s| if s.z.as_ref().map_or(false, |z| eval(z, &inputs, v) == 1) { Z } else { eval(&s.f, &inputs, v) }).collect())
        .collect();
    let got = circuits.iter().map(|(label, c)| (label.clone(), c.simulate(&inputs, &outputs))).collect();
    let named = |p: &String| inputs.iter().chain(&outputs).any(|x| x.eq_ignore_ascii_case(p));
    let unused = circuits
        .first()
        .map(|(_, c)| c.ports.iter().filter(|p| !named(p) && !c.find(p).map_or(false, |i| c.rails.iter().any(|r| r.0 == i))).cloned().collect())
        .unwrap_or_default();
    Ok(Outcome { inputs, outputs, want, got, unused })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cdl::parse_cdl;

    fn circuit(cdl: &str, name: &str) -> Circuit {
        let c = parse_cdl(cdl, "t.cdl");
        Circuit::from_subckt(&crate::cdl::flatten_subckt(&c, name).unwrap())
    }

    fn differ(cdl: &str, name: &str, f: &str) -> usize {
        let c = circuit(cdl, name);
        let o = run(f, &[("s".into(), &c)]).unwrap();
        o.differ(0).unwrap()
    }

    const CELLS: &str = "
.SUBCKT NAND2 A B Y VDD VSS
MP1 Y A VDD VDD pmos w=1u l=0.1u
MP2 Y B VDD VDD pmos w=1u l=0.1u
MN1 Y A n1 VSS nmos w=1u l=0.1u
MN2 n1 B VSS VSS nmos w=1u l=0.1u
.ENDS
.SUBCKT AND2 A B Y VDD VSS
MP1 yb A VDD VDD pmos w=1u l=0.1u
MP2 yb B VDD VDD pmos w=1u l=0.1u
MN1 yb A n1 VSS nmos w=1u l=0.1u
MN2 n1 B VSS VSS nmos w=1u l=0.1u
MP3 Y yb VDD VDD pmos w=1u l=0.1u
MN3 Y yb VSS VSS nmos w=1u l=0.1u
.ENDS
.SUBCKT EINV A EN Z VPWR VGND VPB VNB
MP1 p1 A VPWR VPB pmos w=1u l=0.1u
MP2 Z enb p1 VPB pmos w=1u l=0.1u
MN1 Z EN n1 VNB nmos w=1u l=0.1u
MN2 n1 A VGND VNB nmos w=1u l=0.1u
MP3 enb EN VPWR VPB pmos w=1u l=0.1u
MN3 enb EN VGND VNB nmos w=1u l=0.1u
.ENDS
";

    #[test]
    fn static_gates() {
        assert_eq!(differ(CELLS, "NAND2", "Y = !(A & B)"), 0);
        assert_eq!(differ(CELLS, "NAND2", "Y = !(A B)"), 0);
        assert_eq!(differ(CELLS, "NAND2", "Y = (A' + B')"), 0);
        assert_eq!(differ(CELLS, "NAND2", "Y = A & B"), 4);
        assert_eq!(differ(CELLS, "NAND2", "Y = !(A | B)"), 2);
        assert_eq!(differ(CELLS, "AND2", "Y = A * B"), 0);
    }

    #[test]
    fn pass_transistor_xor() {
        // transmission gates pass B or its inverse to Y
        let tg = "
.SUBCKT XOR2 A B Y VDD VSS
MP1 ab A VDD VDD pmos w=1u l=0.1u
MN1 ab A VSS VSS nmos w=1u l=0.1u
MP2 bb B VDD VDD pmos w=1u l=0.1u
MN2 bb B VSS VSS nmos w=1u l=0.1u
MN3 Y ab B VSS nmos w=1u l=0.1u
MP3 Y A B VDD pmos w=1u l=0.1u
MN4 Y A bb VSS nmos w=1u l=0.1u
MP4 Y ab bb VDD pmos w=1u l=0.1u
.ENDS
";
        assert_eq!(differ(tg, "XOR2", "Y = A ^ B"), 0);
        assert_eq!(differ(tg, "XOR2", "Y = !(A ^ B)"), 4);
    }

    #[test]
    fn three_state_and_supplies_by_name() {
        let c = circuit(CELLS, "EINV");
        assert_eq!(c.rails.len(), 4);
        let o = run("Z = !A\nthree_state(Z) = !EN", &[("s".into(), &c)]).unwrap();
        assert_eq!(o.differ(0), Some(0));
        // without its three_state the output is Z where EN is 0
        let o = run("Z = !A | (EN & !EN)", &[("s".into(), &c)]).unwrap();
        let g = o.got[0].1.as_ref().unwrap();
        assert_eq!(g.iter().filter(|r| r[0] == Z).count(), 2, "{:?} {:?}", g, o.inputs);
        // EN not in the function: left floating, so the output is X, and the port is named
        let o = run("Z = !A", &[("s".into(), &c)]).unwrap();
        assert_eq!(o.differ(0), Some(2));
        assert_eq!(o.got[0].1.as_ref().unwrap().iter().filter(|r| r[0] == X).count(), 2);
        assert_eq!(o.unused, vec!["EN".to_string()]);
    }

    #[test]
    fn errors() {
        let c = circuit(CELLS, "NAND2");
        assert!(run("Y = !(A & C)", &[("s".into(), &c)]).unwrap().got[0].1.is_err());
        assert!(run("Y", &[("s".into(), &c)]).is_err());
        assert!(run("Q = !(D & Q)", &[("s".into(), &c)]).is_err());
        assert!(run("Y = !(A & B", &[("s".into(), &c)]).is_err());
        assert!(parse_spec("three_state(W) = A\nY = B").is_err());
        let s = parse_spec("Y = A ; three_state(Y) = B  # comment").unwrap();
        assert!(s[0].z.is_some());
    }

    #[test]
    fn fight_resolved_by_series_count_stays_x_downstream() {
        // AND2 whose pull-down ignores A: at A=0 B=1 the pmos of A fights the two series nmos of B on yb,
        // so Y is undecided there, it must not read 0 because the shorter path won the fight
        let bad = "
.SUBCKT AND2M A B Y VDD VSS
MP1 yb A VDD VDD pmos w=1u l=0.1u
MP2 yb B VDD VDD pmos w=1u l=0.1u
MN1 yb B n1 VSS nmos w=1u l=0.1u
MN2 n1 B VSS VSS nmos w=1u l=0.1u
MP3 Y yb VDD VDD pmos w=1u l=0.1u
MN3 Y yb VSS VSS nmos w=1u l=0.1u
.ENDS
";
        let c = circuit(bad, "AND2M");
        let o = run("Y = A & B", &[("s".into(), &c)]).unwrap();
        // vectors in order A B = 00 01 10 11: 01 is the fight
        assert_eq!(o.got[0].1.as_ref().unwrap()[1][0], X, "{:?}", o.got);
        assert_eq!(o.differ(0), Some(1));
    }

    #[test]
    fn floating_node_never_decides_by_chance() {
        // OR3 whose pull-down of A is gated by B: at A=1 B=0 C=0 nothing drives y, so X is undecided there even
        // though y happens to end at the same level from both starts in some orders of evaluation
        let bad = "
.SUBCKT OR3M A B C X VDD VSS
MP1 VDD A pa VDD pmos w=1u l=0.1u
MP2 pa B pb VDD pmos w=1u l=0.1u
MP3 pb C y VDD pmos w=1u l=0.1u
MN1 y B VSS VSS nmos w=1u l=0.1u
MN2 y B VSS VSS nmos w=1u l=0.1u
MN3 y C VSS VSS nmos w=1u l=0.1u
MP4 X y VDD VDD pmos w=1u l=0.1u
MN4 X y VSS VSS nmos w=1u l=0.1u
.ENDS
";
        let c = circuit(bad, "OR3M");
        let o = run("X = A | B | C", &[("s".into(), &c)]).unwrap();
        // A B C = 100 is vector 4
        assert_eq!(o.got[0].1.as_ref().unwrap()[4][0], X, "{:?}", o.got);
        assert_eq!(o.differ(0), Some(1));
        // an input left out of the function floats: B and C free, so X = A is undecided wherever A = 0
        let o = run("X = A", &[("s".into(), &c)]).unwrap();
        assert_eq!(o.got[0].1.as_ref().unwrap()[0][0], X, "{:?}", o.got);
    }

    #[test]
    fn two_steady_states_stay_x() {
        // AND2 whose A nmos is gated by Y: at A=B=1 both yb=0,Y=1 and yb=1 (floating, kept),Y=0 are steady
        let fb = "
.SUBCKT AND2F A B Y VDD VSS
MP1 yb A VDD VDD pmos w=1u l=0.1u
MP2 yb B VDD VDD pmos w=1u l=0.1u
MN1 yb B n1 VSS nmos w=1u l=0.1u
MN2 n1 Y VSS VSS nmos w=1u l=0.1u
MP3 Y yb VDD VDD pmos w=1u l=0.1u
MN3 Y yb VSS VSS nmos w=1u l=0.1u
.ENDS
";
        let c = circuit(fb, "AND2F");
        let o = run("Y = A & B", &[("s".into(), &c)]).unwrap();
        assert_eq!(o.got[0].1.as_ref().unwrap()[3][0], X, "{:?}", o.got);
    }

    #[test]
    fn short_between_held_nodes_is_x() {
        // an inverter plus a pmos from VSS to VDD gated by A: at A=0 the supplies are shorted
        let sh = "
.SUBCKT INVS A Y VDD VSS
MP1 Y A VDD VDD pmos w=1u l=0.1u
MN1 Y A VSS VSS nmos w=1u l=0.1u
MP2 VSS A VDD VDD pmos w=1u l=0.1u
.ENDS
";
        let c = circuit(sh, "INVS");
        let g = run("Y = !A", &[("s".into(), &c)]).unwrap().got[0].1.clone().unwrap();
        assert_eq!(g, vec![vec![X], vec![0]]);
    }

    #[test]
    fn cross_coupled_level_shifter_resolves() {
        // A drives one nmos, its inverse the other, the pmos pair is cross-coupled: X = A
        let ls = "
.SUBCKT LS A X VDD VSS
MI1 ab A VDD VDD pmos w=1u l=0.1u
MI2 ab A VSS VSS nmos w=1u l=0.1u
MN1 p A VSS VSS nmos w=2u l=0.1u
MN2 q ab VSS VSS nmos w=2u l=0.1u
MP1 p q VDD VDD pmos w=1u l=0.1u
MP2 q p VDD VDD pmos w=1u l=0.1u
MP3 X p VDD VDD pmos w=1u l=0.1u
MN3 X p VSS VSS nmos w=1u l=0.1u
.ENDS
";
        assert_eq!(differ(ls, "LS", "X = A"), 0);
    }

    #[test]
    fn review_cases() {
        use crate::extract::{ExNet, Extraction};
        // layout nets sharing texts are one node whatever their order
        let sets = [vec!["A"], vec!["B"], vec!["A", "B"]];
        for order in [[0usize, 1, 2], [2, 0, 1], [0, 2, 1]] {
            let ex = Extraction { nets: order.iter().map(|&k| ExNet { names: sets[k].iter().map(|s| s.to_string()).collect(), ..Default::default() }).collect(), ..Default::default() };
            let c = Circuit::from_extraction(&ex, None);
            assert_eq!(c.find("A"), c.find("B"), "{:?}", order);
        }
        // a pin called three_stateY is an output, not three_state(Y)
        let s = parse_spec("three_stateY = A").unwrap();
        assert_eq!(s[0].out, "three_stateY");
        // an output used as an input under another case is a stored state
        let c = circuit(CELLS, "NAND2");
        assert!(run("Y = !(A & y)", &[("s".into(), &c)]).is_err());
        // banks of flip-flops are sequential, three_state naming an output keeps a state
        let bank = LCell { bank: true, ..Default::default() };
        assert!(liberty_text(&bank).unwrap_err().starts_with("sequential"));
        let hold = LCell {
            pins: vec![crate::liberty::LPin { name: "Y".into(), dir: "inout".into(), func: Some("A".into()), three_state: Some("!EN & Y".into()), ..Default::default() }],
            ..Default::default()
        };
        assert!(liberty_text(&hold).unwrap_err().starts_with("holds a state"));
        // supplies named on one net
        let sh = circuit(".SUBCKT SH A Y VDD VSS\nR0 VDD VSS short\nMP1 Y A VDD VDD pmos\nMN1 Y A VSS VSS nmos\n.ENDS\n", "SH");
        let e = run("Y = !A", &[("s".into(), &sh)]).unwrap().got[0].1.clone().unwrap_err();
        assert!(e.contains("short"), "{}", e);
    }

    #[test]
    fn degraded_level_and_keeper() {
        // an nmos with its gate at VDD passes A: a 0 at full swing, a 1 only as VDD minus a threshold
        let pass = ".SUBCKT PASS A Y VDD VSS\nMN1 Y VDD A VSS nmos w=1u l=0.1u\n.ENDS\n";
        let c = circuit(pass, "PASS");
        assert_eq!(run("Y = A", &[("s".into(), &c)]).unwrap().got[0].1.clone().unwrap(), vec![vec![0], vec![X]]);
        // a weak inverter into m with a strong pmos keeper on m gated by Y = !m: at A=1 the keeper can hold m=1
        // against the small nmos (a second state), at A=0 m=1 is the only state
        let keep = "
.SUBCKT KEEP A Y VDD VSS
MP1 m A VDD VDD pmos w=0.4u l=0.1u
MN1 m A VSS VSS nmos w=0.2u l=0.1u
MPK m Y VDD VDD pmos w=4u l=0.1u
MP2 Y m VDD VDD pmos w=1u l=0.1u
MN2 Y m VSS VSS nmos w=1u l=0.1u
.ENDS
";
        let c = circuit(keep, "KEEP");
        assert_eq!(run("Y = A", &[("s".into(), &c)]).unwrap().got[0].1.clone().unwrap(), vec![vec![0], vec![X]]);
        // the same with a keeper weaker than the nmos: m=1 cannot be held at A=1, so Y = A
        let weak = keep.replace("MPK m Y VDD VDD pmos w=4u", "MPK m Y VDD VDD pmos w=0.1u");
        assert_eq!(differ(&weak, "KEEP", "Y = A"), 0);
    }

    #[test]
    fn tie_cells_resolve() {
        let tie = "
.SUBCKT TIEHI H VDD VSS
MN1 n n VSS VSS nmos w=1u l=0.1u
MP1 H n VDD VDD pmos w=1u l=0.1u
.ENDS
.SUBCKT TIELO L VDD VSS
MP1 p p VDD VDD pmos w=1u l=0.1u
MN1 L p VSS VSS nmos w=1u l=0.1u
.ENDS
";
        assert_eq!(differ(tie, "TIEHI", "H = 1"), 0);
        assert_eq!(differ(tie, "TIELO", "L = 0"), 0);
    }

    #[test]
    fn case_of_pin_names() {
        let c = circuit(CELLS, "NAND2");
        assert_eq!(run("y = !(a & b)", &[("s".into(), &c)]).unwrap().differ(0), Some(0));
    }
}
