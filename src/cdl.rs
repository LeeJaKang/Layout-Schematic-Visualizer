//! CDL / SPICE subcircuit reader (port of the web viewer's cdl.js).
use std::collections::HashMap;

pub fn si(s: &str) -> f64 {
    let t = s.trim();
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }
    while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
        i += 1;
    }
    if i < b.len() && (b[i] == b'e' || b[i] == b'E') && i + 1 < b.len() && (b[i + 1].is_ascii_digit() || b[i + 1] == b'-' || b[i + 1] == b'+') {
        i += 2;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
    }
    let v: f64 = match t[..i].parse() {
        Ok(v) => v,
        Err(_) => return f64::NAN,
    };
    let rest = t[i..].to_lowercase();
    let mul = if rest.starts_with("meg") {
        1e6
    } else {
        match rest.chars().next() {
            Some('t') => 1e12,
            Some('g') => 1e9,
            Some('k') => 1e3,
            Some('m') => 1e-3,
            Some('u') => 1e-6,
            Some('n') => 1e-9,
            Some('p') => 1e-12,
            Some('f') => 1e-15,
            Some('a') => 1e-18,
            _ => 1.0,
        }
    };
    v * mul
}

#[derive(Clone, Debug, Default)]
pub struct Mos {
    pub name: String,
    pub typ: String, // nmos | pmos
    pub model: String,
    pub d: String,
    pub g: String,
    pub s: String,
    pub b: String,
    pub w: f64,
    pub l: f64,
    pub nfin: f64,
    pub m: f64,
    pub nf: f64,
}

#[derive(Clone, Debug, Default)]
pub struct XInst {
    pub name: String,
    pub sub: String,
    pub nets: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct Subckt {
    pub name: String,
    pub ports: Vec<String>,
    pub mos: Vec<Mos>,
    pub other: usize,
    pub insts: Vec<XInst>,
    /// zero-ohm resistors (model "short", as SKY130 ties HI/LO and probe outputs): their nets are one net
    pub shorts: Vec<(String, String)>,
    /// MOSFET lines the reader could not use, in plain words with the line number
    pub dropped: Vec<String>,
}

#[derive(Default)]
pub struct Cdl {
    pub subckts: HashMap<String, Subckt>,
    pub order: Vec<String>,
    pub warnings: Vec<String>,
}

pub fn parse_cdl(text: &str, file: &str) -> Cdl {
    let mut lines: Vec<(String, usize)> = vec![];
    for (ln, raw) in text.lines().enumerate() {
        let t = raw.trim_start();
        if t.starts_with('*') {
            continue;
        }
        let mut s = raw.to_string();
        if let Some(k) = s.find(" $") {
            if !s.contains("$[") {
                s.truncate(k);
            }
        }
        if let Some(rest) = s.trim_start().strip_prefix('+') {
            if let Some(last) = lines.last_mut() {
                last.0.push(' ');
                last.0.push_str(rest);
            }
            continue;
        }
        if s.trim().is_empty() {
            continue;
        }
        lines.push((s.trim().to_string(), ln + 1));
    }
    let mut cdl = Cdl::default();
    let mut cur: Option<Subckt> = None;
    for (text, ln) in lines {
        let tok: Vec<&str> = text.split_whitespace().collect();
        let head = tok[0].to_uppercase();
        if head == ".SUBCKT" {
            if let Some(c) = cur.take() {
                cdl.order.push(c.name.clone());
                cdl.subckts.insert(c.name.clone(), c);
            }
            let name = tok.get(1).map(|s| s.to_string()).unwrap_or_default();
            let ports = tok[2..].iter().filter(|t| !t.contains('=') && t.to_uppercase() != "PARAMS:").map(|s| s.to_string()).collect();
            cur = Some(Subckt { name, ports, ..Default::default() });
            continue;
        }
        if head == ".ENDS" {
            if let Some(c) = cur.take() {
                cdl.order.push(c.name.clone());
                cdl.subckts.insert(c.name.clone(), c);
            }
            continue;
        }
        if head.starts_with('.') {
            continue;
        }
        let c = match cur.as_mut() {
            Some(c) => c,
            None => {
                if cdl.warnings.len() < 100 {
                    cdl.warnings.push(format!("{} line {}: element outside .SUBCKT ignored", file, ln));
                }
                continue;
            }
        };
        let mut pos = vec![];
        let mut params: HashMap<String, String> = HashMap::new();
        for t in &tok[1..] {
            if let Some(k) = t.find('=') {
                params.insert(t[..k].to_lowercase(), t[k + 1..].to_string());
            } else if *t != "/" {
                pos.push(t.to_string());
            }
        }
        match head.chars().next().unwrap() {
            'M' => {
                // 3-terminal form "M d g s model" (GT3), accepted only when the fourth token names a MOS model
                let is_model = |t: &str| {
                    let t = t.to_lowercase();
                    t.contains("mos") || t.contains("fet") || t.contains("nch") || t.contains("pch")
                };
                let three = pos.len() == 4 && is_model(&pos[3]);
                if pos.len() < 5 && !three {
                    if cdl.warnings.len() < 100 {
                        cdl.warnings.push(format!("{} line {}: MOSFET {} needs d g s b model", file, ln, tok[0]));
                    }
                    // a 4-field line (one node short) is malformed: SPICE and CDL need drain gate source bulk model
                    c.dropped.push(format!("line {}: MOSFET {} has {} fields ({}) where drain, gate, source, bulk and model are needed, so one node is missing and the device is left out", ln, tok[0], pos.len(), pos.join(" ")));
                    continue;
                }
                let (bulk, model) = if three { (String::new(), pos[3].clone()) } else { (pos[3].clone(), pos[4].clone()) };
                let ml = model.to_lowercase();
                let has_n = ml.contains("nmos") || ml.contains("nfet") || ml.contains("nch");
                let typ = if ml.contains('p') && !has_n { "pmos" } else { "nmos" };
                let get = |k: &str, d: f64| params.get(k).map(|v| si(v)).unwrap_or(d);
                let w = get("w", get("wgaa", f64::NAN));
                c.mos.push(Mos { name: tok[0].to_string(), typ: typ.into(), model, d: pos[0].clone(), g: pos[1].clone(), s: pos[2].clone(), b: bulk, w, l: get("l", f64::NAN), nfin: get("nfin", f64::NAN), m: get("m", 1.0) * get("mult", 1.0), nf: get("nf", 1.0) });
            }
            'X' => {
                let (nets, sub) = match tok.iter().position(|t| *t == "/") {
                    Some(sl) => (tok[1..sl].iter().map(|s| s.to_string()).collect::<Vec<_>>(), tok.get(sl + 1).map(|s| s.to_string()).unwrap_or_default()),
                    None => (pos[..pos.len().saturating_sub(1)].to_vec(), pos.last().cloned().unwrap_or_default()),
                };
                c.insts.push(XInst { name: tok[0].to_string(), sub, nets: nets.into_iter().filter(|x| !x.contains('=')).collect() });
            }
            'R' if pos.len() >= 3 && pos[2].eq_ignore_ascii_case("short") => c.shorts.push((pos[0].clone(), pos[1].clone())),
            'R' | 'C' | 'D' => c.other += 1,
            _ => {
                if cdl.warnings.len() < 100 {
                    cdl.warnings.push(format!("{} line {}: element {} not read", file, ln, tok[0]));
                }
            }
        }
    }
    if let Some(c) = cur.take() {
        cdl.order.push(c.name.clone());
        cdl.subckts.insert(c.name.clone(), c);
    }
    cdl
}

/// a subckt with instances expanded down to MOSFETs; nested device and net names get the instance path
pub fn flatten_subckt(cdl: &Cdl, name: &str) -> Result<Subckt, String> {
    let top = cdl.subckts.get(name).ok_or_else(|| format!("no .SUBCKT {} in the CDL", name))?;
    let mut out = Subckt { name: name.to_string(), ports: top.ports.clone(), ..Default::default() };
    fn walk(cdl: &Cdl, sc: &Subckt, map: &HashMap<String, String>, prefix: &str, depth: usize, out: &mut Subckt) -> Result<(), String> {
        if depth > 32 {
            return Err(format!("CDL hierarchy deeper than 32 at {}", sc.name));
        }
        let net = |n: &str| map.get(n).cloned().unwrap_or_else(|| format!("{}{}", prefix, n));
        for m in &sc.mos {
            out.mos.push(Mos { name: format!("{}{}", prefix, m.name), d: net(&m.d), g: net(&m.g), s: net(&m.s), b: net(&m.b), ..m.clone() });
        }
        for (a, b) in &sc.shorts {
            out.shorts.push((net(a), net(b)));
        }
        out.other += sc.other;
        for d in &sc.dropped {
            out.dropped.push(if prefix.is_empty() { d.clone() } else { format!("{} (in {})", d, prefix.trim_end_matches('/')) });
        }
        for x in &sc.insts {
            let sub = match cdl.subckts.get(&x.sub) {
                Some(s) => s,
                None => continue,
            };
            let mut m = HashMap::new();
            for (i, p) in sub.ports.iter().enumerate() {
                m.insert(p.clone(), match x.nets.get(i) {
                    Some(n) => net(n),
                    None => format!("{}{}/{}", prefix, x.name, p),
                });
            }
            walk(cdl, sub, &m, &format!("{}{}/", prefix, x.name), depth + 1, out)?;
        }
        Ok(())
    }
    let map = top.ports.iter().map(|p| (p.clone(), p.clone())).collect();
    walk(cdl, top, &map, "", 0, &mut out)?;
    merge_shorts(&mut out);
    Ok(out)
}

/// give the two nets of every short one name: a port name before an internal one, the earlier port first
fn merge_shorts(sc: &mut Subckt) {
    if sc.shorts.is_empty() {
        return;
    }
    let rank = |n: &str, ports: &[String]| ports.iter().position(|p| p == n).unwrap_or(usize::MAX);
    let mut rep: HashMap<String, String> = HashMap::new();
    fn root(rep: &HashMap<String, String>, n: &str) -> String {
        let mut x = n.to_string();
        while let Some(y) = rep.get(&x) {
            x = y.clone();
        }
        x
    }
    for (a, b) in sc.shorts.clone() {
        let (ra, rb) = (root(&rep, &a), root(&rep, &b));
        if ra == rb {
            continue;
        }
        let (ka, kb) = (rank(&ra, &sc.ports), rank(&rb, &sc.ports));
        let (keep, drop) = if ka < kb || (ka == kb && ra < rb) { (ra, rb) } else { (rb, ra) };
        rep.insert(drop, keep);
    }
    let f = |n: &String| root(&rep, n);
    for m in sc.mos.iter_mut() {
        m.d = f(&m.d);
        m.g = f(&m.g);
        m.s = f(&m.s);
        m.b = f(&m.b);
    }
    let mut ports: Vec<String> = vec![];
    for p in &sc.ports {
        let r = f(p);
        if !ports.contains(&r) {
            ports.push(r);
        }
    }
    sc.ports = ports;
}
