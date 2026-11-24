//! Elaboration (port of the web viewer's netlist.js): flatten modules into cells, nets and top ports.
use crate::cells::{analyze_cell, unknown_cell, Model};
use crate::liberty::{LCell, LPin};
use crate::verilog::{Expr, Module, PRIMS};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const SEP: char = '\u{1}';

pub fn display_key(k: &str) -> String {
    if !k.contains(SEP) {
        return k.to_string();
    }
    let mut out = String::with_capacity(k.len() + 2);
    let mut it = k.chars().peekable();
    while let Some(c) = it.next() {
        if c == SEP {
            out.push('[');
            while let Some(&d) = it.peek() {
                if d.is_ascii_digit() || d == '-' {
                    out.push(d);
                    it.next();
                } else {
                    break;
                }
            }
            out.push(']');
        } else {
            out.push(c);
        }
    }
    out
}

fn guess_dir(pin: &str) -> &'static str {
    const OUT: [&str; 18] = ["Y", "Z", "ZN", "Q", "QN", "QB", "O", "OUT", "CO", "CON", "S", "SN", "SUM", "X", "GCLK", "ECK", "HI", "LO"];
    let u = pin.to_uppercase();
    if OUT.contains(&u.as_str()) || u == "H" || u == "L" {
        "output"
    } else {
        "input"
    }
}

fn prim_model(typ: &str, nconn: usize) -> Model {
    let ins: Vec<String> = (0..nconn.saturating_sub(1)).map(|i| format!("I{}", i)).collect();
    let inv = matches!(typ, "nand" | "nor" | "xnor" | "not");
    let f = match typ {
        "buf" | "not" => format!("{}I0", if inv { "!" } else { "" }),
        _ => {
            let op = match typ {
                "and" | "nand" => " & ",
                "or" | "nor" => " | ",
                _ => " ^ ",
            };
            let j = ins.join(op);
            if inv { format!("!({})", j) } else { j }
        }
    };
    let mut pins = vec![LPin { name: "Y".into(), dir: "output".into(), func: Some(f), ..Default::default() }];
    pins.extend(ins.iter().map(|n| LPin { name: n.clone(), dir: "input".into(), ..Default::default() }));
    let mut m = analyze_cell(&LCell { name: typ.into(), pins, ..Default::default() });
    m.title = typ.into();
    m
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum End {
    Cell(u32, u16), // cell, pin index in cell.pins
    Port(u32),
    Const(u8),
}

#[derive(Clone, Debug, Default)]
pub struct Net {
    pub id: u32,
    pub keys: Vec<u32>,
    pub drivers: Vec<End>,
    pub sinks: Vec<End>,
    pub name: String,
    pub const_val: Option<u8>,
}

#[derive(Clone, Debug)]
pub struct CellPin {
    pub pin: String,
    pub dir: String,
    pub net: u32,
}

#[derive(Clone, Debug)]
pub struct Cell {
    pub name: String,
    pub typ: String,
    pub model: Arc<Model>,
    pub pins: Vec<CellPin>,
    pub scope: String,
}

#[derive(Clone, Debug)]
pub struct Port {
    pub name: String,
    pub dir: String,
    pub net: u32,
}

#[derive(Clone, Debug)]
pub struct Hier {
    pub path: String,
    pub module: String,
    pub parent: String,
    /// declared ports, one per bit: (name, direction, net)
    pub ports: Vec<(String, String, u32)>,
}

pub struct Netlist {
    pub top: String,
    pub cells: Vec<Cell>,
    pub nets: Vec<Net>,
    pub ports: Vec<Port>,
    pub insts: Vec<Hier>,
    pub keys: Vec<String>,
    pub warnings: Vec<String>,
    pub cell_index: HashMap<String, u32>,
    pub net_by_name: HashMap<String, u32>,
}

impl Netlist {
    pub fn key_display(&self, k: u32) -> String {
        display_key(&self.keys[k as usize])
    }
}

#[derive(Clone, Debug)]
enum Bit {
    Key(u32),
    C(u8),
}

struct E<'a> {
    mods: &'a HashMap<String, Module>,
    models: &'a HashMap<String, Arc<Model>>,
    keys: Vec<String>,
    kidx: HashMap<String, u32>,
    parent: Vec<u32>,
    const_of: Vec<(u32, u8)>,
    const_map: HashMap<u32, usize>,
    cells: Vec<(String, String, Vec<(String, Bit)>, String)>, // name, model key, pins, scope
    unknown: HashMap<String, Model>,
    prims: HashMap<String, Model>,
    hier: Vec<Hier>,
    warnings: Vec<String>,
    implicit: HashMap<String, HashSet<String>>, // module -> implicit nets
    const_uses: usize,
}

impl<'a> E<'a> {
    fn warn(&mut self, m: String) {
        if self.warnings.len() < 200 {
            self.warnings.push(m);
        }
    }
    fn key(&mut self, k: String) -> u32 {
        if let Some(&i) = self.kidx.get(&k) {
            return i;
        }
        let i = self.keys.len() as u32;
        self.kidx.insert(k.clone(), i);
        self.keys.push(k);
        self.parent.push(i);
        i
    }
    fn find(&mut self, mut x: u32) -> u32 {
        let mut r = x;
        while self.parent[r as usize] != r {
            r = self.parent[r as usize];
        }
        while self.parent[x as usize] != r {
            let nx = self.parent[x as usize];
            self.parent[x as usize] = r;
            x = nx;
        }
        r
    }
    fn union(&mut self, a: u32, b: u32) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra != rb {
            self.parent[rb as usize] = ra;
        }
    }
    fn set_const(&mut self, k: u32, v: u8) {
        match self.const_map.get(&k) {
            Some(&i) => self.const_of[i].1 = v,
            None => {
                self.const_map.insert(k, self.const_of.len());
                self.const_of.push((k, v));
            }
        }
    }
    fn sig_bits(&self, m: &Module, name: &str) -> Option<(bool, Vec<Option<i64>>)> {
        let d = match m.decls.get(name) {
            Some(d) => d,
            None => {
                if self.implicit.get(&m.name).map(|s| s.contains(name)).unwrap_or(false) {
                    return Some((false, vec![None]));
                }
                return None;
            }
        };
        match (d.msb, d.lsb) {
            (Some(a), Some(b)) => {
                let step = if a >= b { -1 } else { 1 };
                let mut v = vec![];
                let mut x = a;
                loop {
                    v.push(Some(x));
                    if x == b {
                        break;
                    }
                    x += step;
                }
                Some((true, v))
            }
            _ => Some((false, vec![None])),
        }
    }
    fn kname(prefix: &str, name: &str, bit: Option<i64>) -> String {
        match bit {
            None => format!("{}{}", prefix, name),
            Some(b) => format!("{}{}{}{}", prefix, name, SEP, b),
        }
    }
    fn bits_of(&mut self, m: &Module, prefix: &str, e: &Expr) -> Vec<Bit> {
        match e {
            Expr::Const(b) => b.iter().map(|&c| Bit::C(c)).collect(),
            Expr::Concat(p) => p.iter().flat_map(|x| self.bits_of(m, prefix, x)).collect(),
            Expr::Repl(n, p) => {
                let one: Vec<Bit> = p.iter().flat_map(|x| self.bits_of(m, prefix, x)).collect();
                (0..*n).flat_map(|_| one.clone()).collect()
            }
            Expr::Id { name, idx, range } => {
                let info = match self.sig_bits(m, name) {
                    Some(i) => i,
                    None => {
                        self.implicit.entry(m.name.clone()).or_default().insert(name.clone());
                        (false, vec![None])
                    }
                };
                if let Some(i) = idx {
                    if !info.0 {
                        self.warn(format!("{}: {}[{}] selects a bit of a scalar", m.name, name, i));
                        return vec![Bit::Key(self.key(Self::kname(prefix, name, None)))];
                    }
                    if !info.1.contains(&Some(*i)) {
                        self.warn(format!("{}: {}[{}] is outside the declared range", m.name, name, i));
                    }
                    return vec![Bit::Key(self.key(Self::kname(prefix, name, Some(*i))))];
                }
                if let Some((a, b)) = range {
                    let step = if a >= b { -1 } else { 1 };
                    let mut out = vec![];
                    let mut x = *a;
                    loop {
                        out.push(Bit::Key(self.key(Self::kname(prefix, name, if info.0 { Some(x) } else { None }))));
                        if x == *b {
                            break;
                        }
                        x += step;
                    }
                    return out;
                }
                info.1.iter().map(|&b| Bit::Key(self.key(Self::kname(prefix, name, b)))).collect::<Vec<_>>()
            }
        }
    }
    fn connect(&mut self, a: &[Bit], b: &[Bit], where_: &str) {
        let n = a.len().min(b.len());
        if a.len() != b.len() {
            self.warn(format!("{}: width {} connected to width {}", where_, a.len(), b.len()));
        }
        for i in 1..=n {
            match (&a[a.len() - i], &b[b.len() - i]) {
                (Bit::Key(x), Bit::Key(y)) => self.union(*x, *y),
                (Bit::Key(x), Bit::C(c)) => self.set_const(*x, *c),
                (Bit::C(c), Bit::Key(y)) => self.set_const(*y, *c),
                _ => {}
            }
        }
    }
    fn instantiate(&mut self, mname: &str, prefix: &str, depth: usize) -> Result<(), String> {
        if depth > 64 {
            return Err("module hierarchy is deeper than 64 levels (recursive instantiation?)".into());
        }
        let mods = self.mods;
        let m = &mods[mname];
        for name in &m.decl_order {
            let (_, bits) = self.sig_bits(m, name).unwrap();
            for b in bits {
                self.key(Self::kname(prefix, name, b));
            }
        }
        for a in &m.assigns {
            let l = self.bits_of(m, prefix, &a.lhs);
            let r = self.bits_of(m, prefix, &a.rhs);
            self.connect(&l, &r, &format!("{} line {}", m.name, a.line));
        }
        for inst in &m.insts {
            let iname = format!("{}{}", prefix, inst.name);
            let scope = prefix.trim_end_matches('/').to_string();
            if inst.prim && PRIMS.contains(&inst.typ.as_str()) {
                let conns = inst.positional.clone().unwrap_or_default();
                let mk = format!("prim:{}:{}", inst.typ, conns.len());
                if !self.prims.contains_key(&mk) {
                    self.prims.insert(mk.clone(), prim_model(&inst.typ, conns.len()));
                }
                let mut pins = vec![];
                for (i, e) in conns.iter().enumerate() {
                    let pin = if i == 0 { "Y".to_string() } else { format!("I{}", i - 1) };
                    if let Some(e) = e {
                        let bits = self.bits_of(m, prefix, e);
                        if let Some(b) = bits.last() {
                            pins.push((pin, b.clone()));
                        }
                    }
                }
                self.cells.push((iname, mk, pins, scope));
                continue;
            }
            if mods.contains_key(&inst.typ) {
                let sub = &mods[&inst.typ];
                let cp = format!("{}{}/", prefix, inst.name);
                let hi = self.hier.len();
                self.hier.push(Hier { path: format!("{}{}", prefix, inst.name), module: sub.name.clone(), parent: scope.clone(), ports: vec![] });
                self.instantiate(&inst.typ, &cp, depth + 1)?;
                // port bits of the instance, the key index becomes the net once nets exist
                for pn in &sub.ports {
                    if let Some(d) = sub.decls.get(pn).and_then(|d| d.dir.clone()) {
                        let (_, bits) = self.sig_bits(sub, pn).unwrap();
                        for b in bits {
                            let k = self.key(Self::kname(&cp, pn, b));
                            self.hier[hi].ports.push((display_key(&Self::kname("", pn, b)), d.clone(), k));
                        }
                    }
                }
                let list: Vec<(Option<String>, Option<Expr>)> = match &inst.named {
                    Some(n) => n.iter().map(|(p, e)| (Some(p.clone()), e.clone())).collect(),
                    None => inst.positional.clone().unwrap_or_default().into_iter().enumerate().map(|(i, e)| (sub.ports.get(i).cloned(), e)).collect(),
                };
                for (pin, e) in list {
                    let pin = match pin {
                        Some(p) => p,
                        None => {
                            self.warn(format!("{}: more positional connections than ports of {}", iname, sub.name));
                            continue;
                        }
                    };
                    if !sub.decls.contains_key(&pin) {
                        self.warn(format!("{}: {} has no port {}", iname, sub.name, pin));
                        continue;
                    }
                    if let Some(e) = e {
                        let a = self.bits_of(sub, &cp, &Expr::Id { name: pin.clone(), idx: None, range: None });
                        let b = self.bits_of(m, prefix, &e);
                        self.connect(&a, &b, &format!("{}.{}", iname, pin));
                    }
                }
                continue;
            }
            // library cell (or unknown)
            let known = self.models.contains_key(&inst.typ);
            if !known {
                if !self.unknown.contains_key(&inst.typ) {
                    let names: Vec<String> = match &inst.named {
                        Some(n) => n.iter().map(|x| x.0.clone()).collect(),
                        None => (0..inst.positional.as_ref().map(|p| p.len()).unwrap_or(0)).map(|i| format!("P{}", i)).collect(),
                    };
                    self.unknown.insert(inst.typ.clone(), unknown_cell(&inst.typ, &names, guess_dir));
                    self.warn(format!("cell type {} is not in the loaded libraries; pin directions are guessed from pin names", inst.typ));
                }
                if let Some(n) = &inst.named {
                    let u = self.unknown.get_mut(&inst.typ).unwrap();
                    for (p, _) in n {
                        if !u.pin_dir.contains_key(p) {
                            let d = guess_dir(p);
                            u.pin_dir.insert(p.clone(), d.into());
                            if d == "output" { u.outputs.push(p.clone()) } else { u.inputs.push(p.clone()) }
                        }
                    }
                }
            }
            let model: Model = if known { (*self.models[&inst.typ]).clone() } else { self.unknown[&inst.typ].clone() };
            let pin_list: Vec<String> = model.inputs.iter().chain(model.outputs.iter()).chain(model.inouts.iter()).cloned().collect();
            let list: Vec<(Option<String>, Option<Expr>)> = match &inst.named {
                Some(n) => n.iter().map(|(p, e)| (Some(p.clone()), e.clone())).collect(),
                None => {
                    if inst.positional.as_ref().map(|p| !p.is_empty()).unwrap_or(false) {
                        self.warn(format!("{}: positional connections to a library cell follow the Liberty pin order", iname));
                    }
                    inst.positional.clone().unwrap_or_default().into_iter().enumerate().map(|(i, e)| (pin_list.get(i).cloned(), e)).collect()
                }
            };
            let mut pins: Vec<(String, Bit)> = vec![];
            let set_pin = |s: &mut Self, pins: &mut Vec<(String, Bit)>, pn: String, b: Bit| {
                let b = match b {
                    Bit::C(c) => {
                        let k = s.key(format!("#const{}", s.const_uses));
                        s.const_uses += 1;
                        s.set_const(k, c);
                        Bit::Key(k)
                    }
                    k => k,
                };
                if let Some(x) = pins.iter_mut().find(|x| x.0 == pn) {
                    x.1 = b;
                } else {
                    pins.push((pn, b));
                }
            };
            for (pin, e) in list {
                let pin = match pin {
                    Some(p) => p,
                    None => continue,
                };
                if model.pg_pins.contains(&pin) {
                    continue;
                }
                let e = match e {
                    Some(e) => e,
                    None => continue,
                };
                if !model.pin_dir.contains_key(&pin) {
                    let pu = pin.to_uppercase();
                    if ["VDD", "VSS", "VPWR", "VGND", "VPB", "VNB", "VCC", "GND", "VNW", "VPW", "VDDPE", "VDDCE", "VBP", "VBN"].contains(&pu.as_str()) {
                        continue;
                    }
                    let prefix_b = format!("{}[", pin);
                    let mut bus: Vec<(i64, String)> = model.pin_dir.keys().filter_map(|p| p.strip_prefix(&prefix_b).and_then(|r| r.strip_suffix(']')).and_then(|n| n.parse::<i64>().ok()).map(|n| (n, p.clone()))).collect();
                    if !bus.is_empty() {
                        bus.sort_by(|a, b| b.0.cmp(&a.0));
                        let bits = self.bits_of(m, prefix, &e);
                        if bits.len() != bus.len() {
                            self.warn(format!("{}.{}: {}-bit connection to a {}-bit bus, aligned at the LSB", iname, pin, bits.len(), bus.len()));
                        }
                        for q in 1..=bits.len().min(bus.len()) {
                            set_pin(self, &mut pins, bus[bus.len() - q].1.clone(), bits[bits.len() - q].clone());
                        }
                        continue;
                    }
                    self.warn(format!("{}: {} has no pin {}", iname, inst.typ, pin));
                    continue;
                }
                let bits = self.bits_of(m, prefix, &e);
                if bits.is_empty() {
                    continue;
                }
                if bits.len() > 1 {
                    self.warn(format!("{}.{}: {}-bit connection to a 1-bit pin, using the LSB", iname, pin, bits.len()));
                }
                set_pin(self, &mut pins, pin, bits[bits.len() - 1].clone());
            }
            self.cells.push((iname, inst.typ.clone(), pins, scope));
        }
        Ok(())
    }
}

pub fn elaborate(mods: &HashMap<String, Module>, top: &str, models: &HashMap<String, Arc<Model>>) -> Result<Netlist, String> {
    let tm = mods.get(top).ok_or_else(|| format!("top module {} not found", top))?;
    let mut e = E { mods, models, keys: vec![], kidx: HashMap::new(), parent: vec![], const_of: vec![], const_map: HashMap::new(), cells: vec![], unknown: HashMap::new(), prims: HashMap::new(), hier: vec![], warnings: vec![], implicit: HashMap::new(), const_uses: 0 };
    e.instantiate(top, "", 0)?;
    let mut top_ports: Vec<(String, String, u32)> = vec![];
    for p in &tm.ports {
        let d = match tm.decls.get(p).and_then(|d| d.dir.clone()) {
            Some(d) => d,
            None => {
                e.warn(format!("top port {} has no direction", p));
                continue;
            }
        };
        let (_, bits) = e.sig_bits(tm, p).unwrap();
        for b in bits {
            let k = e.key(E::kname("", p, b));
            top_ports.push((display_key(&E::kname("", p, b)), d.clone(), k));
        }
    }
    // nets
    let nk = e.keys.len();
    let mut root_net: HashMap<u32, u32> = HashMap::new();
    let mut nets: Vec<Net> = vec![];
    let mut net_of_key = vec![0u32; nk];
    for k in 0..nk as u32 {
        let r = e.find(k);
        let id = *root_net.entry(r).or_insert_with(|| {
            nets.push(Net { id: nets.len() as u32, ..Default::default() });
            nets.len() as u32 - 1
        });
        nets[id as usize].keys.push(k);
        net_of_key[k as usize] = id;
    }
    let mut warns = vec![];
    for &(k, v) in &e.const_of {
        let n = &mut nets[net_of_key[k as usize] as usize];
        if let Some(old) = n.const_val {
            if old != v {
                warns.push(format!("net {} is tied to both {} and {}", display_key(&e.keys[k as usize]), old as char, v as char));
            }
        }
        n.const_val = Some(v);
    }
    let port_keys: HashSet<u32> = top_ports.iter().map(|p| p.2).collect();
    for n in &mut nets {
        let ks: Vec<u32> = n.keys.iter().copied().filter(|&k| !e.keys[k as usize].starts_with("#const")).collect();
        let mut best = ks.iter().copied().find(|k| port_keys.contains(k));
        if best.is_none() {
            let flat: Vec<u32> = ks.iter().copied().filter(|&k| !e.keys[k as usize].contains('/')).collect();
            let mut pool = if flat.is_empty() { ks.clone() } else { flat };
            pool.sort_by(|&a, &b| {
                let (x, y) = (&e.keys[a as usize], &e.keys[b as usize]);
                x.len().cmp(&y.len()).then_with(|| x.cmp(y))
            });
            best = pool.first().copied();
        }
        n.name = match best {
            Some(b) => display_key(&e.keys[b as usize]),
            None => match n.const_val {
                Some(v) => format!("1'b{}", v as char),
                None => format!("net{}", n.id),
            },
        };
    }
    // cells
    let mut all_models: HashMap<String, Arc<Model>> = HashMap::new();
    for (k, m) in e.unknown.drain() {
        all_models.insert(k, Arc::new(m));
    }
    for (k, m) in e.prims.drain() {
        all_models.insert(k, Arc::new(m));
    }
    let mut cells = Vec::with_capacity(e.cells.len());
    let raw = std::mem::take(&mut e.cells);
    for (ci, (name, mk, pins, scope)) in raw.into_iter().enumerate() {
        let model = all_models.get(&mk).cloned().or_else(|| models.get(&mk).cloned()).unwrap();
        let mut cp = vec![];
        for (pin, b) in pins {
            let k = match b {
                Bit::Key(k) => k,
                Bit::C(_) => continue,
            };
            let net = net_of_key[k as usize];
            let dir = model.pin_dir.get(&pin).cloned().unwrap_or_else(|| "input".into());
            let pi = cp.len() as u16;
            let end = End::Cell(ci as u32, pi);
            match dir.as_str() {
                "output" => nets[net as usize].drivers.push(end),
                "inout" => {
                    nets[net as usize].drivers.push(end);
                    nets[net as usize].sinks.push(end);
                }
                _ => nets[net as usize].sinks.push(end),
            }
            cp.push(CellPin { pin, dir, net });
        }
        let typ = if mk.starts_with("prim:") { mk.split(':').nth(1).unwrap().to_string() } else { mk };
        cells.push(Cell { name, typ, model, pins: cp, scope });
    }
    let mut ports = vec![];
    for (i, (name, dir, k)) in top_ports.into_iter().enumerate() {
        let net = net_of_key[k as usize];
        let end = End::Port(i as u32);
        match dir.as_str() {
            "input" => nets[net as usize].drivers.push(end),
            "output" => nets[net as usize].sinks.push(end),
            _ => {
                nets[net as usize].drivers.push(end);
                nets[net as usize].sinks.push(end);
            }
        }
        ports.push(Port { name, dir, net });
    }
    for n in &mut nets {
        if let Some(v) = n.const_val {
            if n.drivers.iter().any(|d| matches!(d, End::Cell(..))) {
                warns.push(format!("net {} has a constant and a cell driver", n.name));
            }
            n.drivers.push(End::Const(v));
        }
        let cd = n.drivers.iter().filter(|d| matches!(d, End::Cell(..))).count();
        if cd > 1 {
            warns.push(format!("net {} has {} drivers", n.name, n.drivers.len()));
        }
    }
    for w in warns {
        e.warn(w);
    }
    for h in &mut e.hier {
        for p in &mut h.ports {
            p.2 = net_of_key[p.2 as usize];
        }
    }
    let cell_index = cells.iter().enumerate().map(|(i, c)| (c.name.clone(), i as u32)).collect();
    let mut net_by_name = HashMap::new();
    for n in &nets {
        for &k in &n.keys {
            let s = &e.keys[k as usize];
            if !s.starts_with('#') {
                net_by_name.insert(display_key(s), n.id);
            }
        }
    }
    Ok(Netlist { top: top.to_string(), cells, nets, ports, insts: e.hier, keys: e.keys, warnings: e.warnings, cell_index, net_by_name })
}

pub fn pick_top(order: &[String], mods: &HashMap<String, Module>) -> Vec<String> {
    let used: HashSet<&String> = mods.values().flat_map(|m| m.insts.iter().map(|i| &i.typ)).collect();
    let roots: Vec<String> = order.iter().filter(|n| !used.contains(n)).cloned().collect();
    if roots.is_empty() { order.to_vec() } else { roots }
}

/// cells reachable from a start set within `back` driver steps and `fwd` sink steps
pub fn cone_cells(nl: &Netlist, start: &[u32], back: usize, fwd: usize) -> Vec<u32> {
    let mut keep: HashSet<u32> = start.iter().copied().collect();
    let mut order: Vec<u32> = start.to_vec();
    let mut frontier = start.to_vec();
    for _ in 0..back {
        let mut nx = vec![];
        for &c in &frontier {
            for p in &nl.cells[c as usize].pins {
                if p.dir == "output" {
                    continue;
                }
                for d in &nl.nets[p.net as usize].drivers {
                    if let End::Cell(dc, _) = d {
                        if keep.insert(*dc) {
                            nx.push(*dc);
                            order.push(*dc);
                        }
                    }
                }
            }
        }
        frontier = nx;
    }
    frontier = start.to_vec();
    for _ in 0..fwd {
        let mut nx = vec![];
        for &c in &frontier {
            for p in &nl.cells[c as usize].pins {
                if p.dir == "input" {
                    continue;
                }
                for s in &nl.nets[p.net as usize].sinks {
                    if let End::Cell(sc, _) = s {
                        if keep.insert(*sc) {
                            nx.push(*sc);
                            order.push(*sc);
                        }
                    }
                }
            }
        }
        frontier = nx;
    }
    order
}
