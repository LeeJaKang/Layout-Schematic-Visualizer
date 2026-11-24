//! Post-layout (LPE) SPICE reader and parasitic layer attribution.
//!
//! Reading: a generic SPICE reader with `+` continuation lines, `*` comment lines, `$key=value` tail
//! comments and `.include` (a missing path is retried as the same file name next to the including file,
//! then next to the main file). A split netlist (main file with the MOSFETs, a `.pex` file with one
//! `PM_<CELL>%<NET>` subckt of R and ground C per net, a `.pxi` file with the PM instances and the
//! coupling caps), Magic ext2spice output and flat netlists with R and C directly in the cell subckt read the same way.
//! DSPF: each `*|NET`
//! section names its net and its `*|P`, `*|I`, `*|S` points are the node locations ((0, 0) there means none).
//! The R and C lines and the Instance Section are ordinary SPICE. `$lvl=` tags map through `*LAYER_MAP`, a
//! `*|GROUND_NET` other than a port is ground, `// layer1= layer2=` comments tag the two sides of a C.
//!
//! Model: every top cell (a subckt that holds MOSFETs, directly or below, and that no other subckt
//! instantiates) is flattened. Nets are the connected components of nodes joined by resistors and by
//! membership in one PM instance. A net is named by the PM `%NET` suffix, else by the first top
//! port in it, else by its first node. A port like `VSS__2` that only MOSFET bulks use is an alias of
//! `VSS`. The cell becomes a `cdl::Subckt` (device terminals collapsed to net names) for the LVS matcher
//! and the transistor schematic, and keeps every R, C and node. Nodes inside a PM instance are
//! named `<NET>:<local>` (other instances: `<instance>/<local>`). Elements keep their local names unless
//! two flattened elements share one, then they get `<instance>/<name>`.
//!
//! Layer attribution: see [`attribute_layers`]. Use from the viewer:
//! `let f = read_lpe(path)?;` then per cell `let m = devmatch::match_cell(&ex, &cell.subckt, true);`
//! `let at = attribute_layers(cell, &ex, &m, &flat, gds.meters_per_db, &rules);` with the same GDS
//! flattening, extraction and rules the CDL path uses. `compare_to_cdl` checks the LPE devices
//! against a CDL subckt.
use crate::cdl::{si, Mos, Subckt};
use crate::devmatch::MatchResult;
use crate::extract::{parse_rules, Extraction};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------- public model

#[derive(Clone, Debug, Default)]
pub struct LpeNode {
    pub name: String,
    /// index into `LpeCell::nets`
    pub net: usize,
    /// location in µm, from the first C, Cc or R line that gives one for this node
    pub xy: Option<[f64; 2]>,
    /// device terminals on this node: (index into `LpeCell::devices` and `subckt.mos`, 'd' | 'g' | 's' | 'b')
    pub term: Vec<(usize, char)>,
}

#[derive(Clone, Debug, Default)]
pub struct LpeNet {
    pub name: String,
    pub nodes: Vec<usize>,
    /// named by a `PM_<CELL>%<NET>` subckt
    pub from_pm: bool,
}

#[derive(Clone, Debug, Default)]
pub struct LpeRes {
    pub name: String,
    pub a: usize,
    pub b: usize,
    pub ohms: f64,
    pub net: usize,
    /// a `$layer=` / `$lvl=` / `$l=<name>` tail tag
    pub tag: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LpeCap {
    pub name: String,
    pub a: usize,
    /// None: to ground (node 0)
    pub b: Option<usize>,
    pub farads: f64,
    /// `$X $Y` (first node) and `$X2 $Y2` (second node), µm
    pub xy: Option<[f64; 2]>,
    pub xy2: Option<[f64; 2]>,
    pub net_a: usize,
    pub net_b: Option<usize>,
    pub coupling: bool,
    pub tag_a: Option<String>,
    pub tag_b: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct LpeDevice {
    pub name: String,
    /// d g s b node indices
    pub nodes: [usize; 4],
    /// `$X $Y` of the device line, µm
    pub xy: Option<[f64; 2]>,
}

#[derive(Clone, Debug, Default)]
pub struct LpeCell {
    pub name: String,
    /// the ports as written on the .subckt line (bulk aliases included)
    pub ports: Vec<String>,
    /// devices with terminals collapsed to net names, ports with aliases merged
    pub subckt: Subckt,
    pub nodes: Vec<LpeNode>,
    pub nets: Vec<LpeNet>,
    pub res: Vec<LpeRes>,
    pub caps: Vec<LpeCap>,
    /// same order as `subckt.mos`
    pub devices: Vec<LpeDevice>,
    /// (alias port, port it was merged into)
    pub aliases: Vec<(String, String)>,
    pub warnings: Vec<String>,
    /// facts about the reading (DSPF sections, points read as none, total C check)
    pub notes: Vec<String>,
}

impl LpeCell {
    pub fn node_index(&self, name: &str) -> Option<usize> {
        self.nodes.iter().position(|n| n.name == name)
    }
    pub fn net_index(&self, name: &str) -> Option<usize> {
        self.nets.iter().position(|n| n.name == name)
    }
}

#[derive(Clone, Debug, Default)]
pub struct LpeFile {
    pub cells: Vec<LpeCell>,
    /// every file read, main file first
    pub files: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------- cards

#[derive(Clone, Debug, Default)]
struct Card {
    toks: Vec<String>,
    tail: Vec<(String, String)>,
    file: String,
    line: usize,
}

fn tokenize(s: &str, toks: &mut Vec<String>, tail: &mut Vec<(String, String)>) {
    let mut words: Vec<String> = vec![];
    let mut comment = false;
    for w in s.split_whitespace() {
        if comment || w.starts_with("//") {
            // after `//` only key=value words are kept (CLAYER `layer1=`, LAYERMAP `$lvl=`)
            let w = if comment { w } else { &w[2..] };
            comment = true;
            let kv = w.strip_prefix('$').unwrap_or(w);
            if let Some(k) = kv.find('=').filter(|&k| k > 0) {
                tail.push((kv[..k].to_lowercase(), kv[k + 1..].to_string()));
            }
            continue;
        }
        if let Some(kv) = w.strip_prefix('$') {
            match kv.find('=') {
                Some(k) if k > 0 => {
                    tail.push((kv[..k].to_lowercase(), kv[k + 1..].to_string()));
                    continue;
                }
                // a bare `$` or `$text`: the rest of the line is a comment
                _ => break,
            }
        }
        words.push(w.to_string());
    }
    // join `a = b`, `a= b`, `a =b`
    let mut i = 0;
    while i < words.len() {
        let w = &words[i];
        if w == "=" && !toks.is_empty() && i + 1 < words.len() {
            let l = toks.pop().unwrap();
            toks.push(format!("{}={}", l, words[i + 1]));
            i += 2;
            continue;
        }
        if w.ends_with('=') && w.len() > 1 && i + 1 < words.len() {
            toks.push(format!("{}{}", w, words[i + 1]));
            i += 2;
            continue;
        }
        if w.starts_with('=') && w.len() > 1 && !toks.is_empty() {
            let l = toks.pop().unwrap();
            toks.push(format!("{}{}", l, w));
            i += 1;
            continue;
        }
        toks.push(w.clone());
        i += 1;
    }
}

/// A DSPF comment line as a card: `*|NET n c` -> ["*|NET", n, c], `*|P (..)`, `*|I (..)`, `*|S (..)` -> the words
/// inside the parentheses after "*|P" etc. (a `// $lvl=..` comment after them goes to the tail), other `*|KEY`
/// lines -> ["*|KEY", words..]. `*LAYER_MAP` (or `*|LAYER_MAP`) starts a list of `*<number> <layer>` lines, each
/// becomes ["*|LAYERMAP", number, layer]. None for ordinary comments.
fn dspf_line(t: &str, in_map: &mut bool) -> Option<(Vec<String>, Vec<(String, String)>)> {
    let body = t.trim_start_matches('*').trim_start();
    let body = body.strip_prefix('|').map(|b| b.trim_start()).unwrap_or(body);
    if body.eq_ignore_ascii_case("LAYER_MAP") {
        *in_map = true;
        return None;
    }
    if *in_map {
        let w: Vec<&str> = body.split_whitespace().collect();
        if w.len() >= 2 && w[0].bytes().all(|b| b.is_ascii_digit()) {
            return Some((vec!["*|LAYERMAP".into(), w[0].into(), w[1].into()], vec![]));
        }
        *in_map = false;
    }
    let rest = t.strip_prefix("*|")?.trim_start();
    let key: String = rest.chars().take_while(|c| !c.is_whitespace() && *c != '(').collect();
    if key.is_empty() {
        return None;
    }
    let key = key.to_uppercase();
    let mut toks = vec![format!("*|{}", key)];
    let mut tail = vec![];
    let after_key = &rest[rest.find(|c: char| c.is_whitespace() || c == '(').unwrap_or(rest.len())..];
    if matches!(key.as_str(), "P" | "I" | "S") {
        let (open, close) = (after_key.find('(')?, after_key.find(')')?);
        if close < open {
            return None;
        }
        toks.extend(after_key[open + 1..close].split_whitespace().map(|w| w.to_string()));
        let mut ignore = vec![];
        tokenize(&after_key[close + 1..], &mut ignore, &mut tail);
    } else {
        toks.extend(after_key.split_whitespace().map(|w| w.to_string()));
    }
    Some((toks, tail))
}

struct Reader<'a> {
    main_dir: PathBuf,
    files: Vec<PathBuf>,
    warnings: Vec<String>,
    load: &'a mut dyn FnMut(&Path) -> Result<String, String>,
}

impl Reader<'_> {
    fn resolve(&self, inc: &str, from: &Path) -> Option<PathBuf> {
        let p = PathBuf::from(inc);
        let dir = from.parent().map(|d| d.to_path_buf()).unwrap_or_default();
        let mut tries = vec![];
        if p.is_absolute() {
            tries.push(p.clone());
        } else {
            tries.push(dir.join(&p));
            tries.push(p.clone());
        }
        if let Some(base) = p.file_name() {
            tries.push(dir.join(base));
            tries.push(self.main_dir.join(base));
        }
        tries.into_iter().find(|t| t.is_file())
    }

    fn read(&mut self, path: &Path, text: &str, depth: usize, out: &mut Vec<Card>) -> Result<(), String> {
        if depth > 16 {
            return Err(format!("{}: .include nested deeper than 16", path.display()));
        }
        let file = path.display().to_string();
        let mut last: Option<usize> = None;
        let mut in_map = false;
        for (ln, raw) in text.lines().enumerate() {
            let t = raw.trim();
            if t.starts_with('*') {
                // DSPF `*|` lines and the `*LAYER_MAP` list become cards, other comments are skipped
                if let Some((toks, tail)) = dspf_line(t, &mut in_map) {
                    out.push(Card { toks, tail, file: file.clone(), line: ln + 1 });
                    last = None;
                }
                continue;
            }
            if t.is_empty() {
                continue;
            }
            in_map = false;
            if let Some(rest) = t.strip_prefix('+') {
                match last {
                    Some(k) => {
                        let c = &mut out[k];
                        tokenize(rest, &mut c.toks, &mut c.tail);
                    }
                    None => self.warnings.push(format!("{} line {}: continuation line without a card", file, ln + 1)),
                }
                continue;
            }
            let mut c = Card { file: file.clone(), line: ln + 1, ..Default::default() };
            let low = t.to_lowercase();
            if low.starts_with(".include") || low.starts_with(".inc ") || low.starts_with(".inc\t") || low.starts_with(".inc\"") || low.starts_with(".inc'") {
                let arg = t.splitn(2, |ch: char| ch.is_whitespace() || ch == '"' || ch == '\'').nth(1).unwrap_or("").trim().trim_matches(|ch| ch == '"' || ch == '\'').trim();
                if arg.is_empty() {
                    return Err(format!("{} line {}: .include without a file name", file, ln + 1));
                }
                let p = self.resolve(arg, path).ok_or_else(|| {
                    format!("{} line {}: .include {} not found (also looked for {} next to {} and next to the main file)", file, ln + 1, arg, Path::new(arg).file_name().map(|b| b.to_string_lossy().to_string()).unwrap_or_default(), file)
                })?;
                if p != Path::new(arg) {
                    self.warnings.push(format!("{} line {}: .include {} read from {}", file, ln + 1, arg, p.display()));
                }
                let sub = (self.load)(&p)?;
                self.files.push(p.clone());
                self.read(&p, &sub, depth + 1, out)?;
                last = None;
                continue;
            }
            if low == ".end" || low.starts_with(".end ") {
                break;
            }
            tokenize(t, &mut c.toks, &mut c.tail);
            if c.toks.is_empty() {
                continue;
            }
            out.push(c);
            last = Some(out.len() - 1);
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- elements

#[derive(Clone, Debug)]
enum Elem {
    Mos { name: String, nodes: [String; 4], model: String, params: HashMap<String, String>, xy: Option<[f64; 2]> },
    Res { name: String, a: String, b: String, ohms: f64, tag: Option<String>, xy: Option<[f64; 2]>, xy2: Option<[f64; 2]> },
    Cap { name: String, a: String, b: String, farads: f64, tag_a: Option<String>, tag_b: Option<String>, xy: Option<[f64; 2]>, xy2: Option<[f64; 2]> },
    Inst { name: String, nodes: Vec<String>, sub: String },
}

#[derive(Clone, Debug, Default)]
struct RawSub {
    name: String,
    ports: Vec<String>,
    elems: Vec<Elem>,
    other: usize,
    /// DSPF `*|NET` sections
    dnets: Vec<DNet>,
    /// DSPF `*|GROUND_NET`
    ground: Option<String>,
}

/// a DSPF `*|NET` section: the net, its total capacitance and the nodes its `*|P`, `*|I`, `*|S` lines list
#[derive(Clone, Debug, Default)]
struct DNet {
    name: String,
    total: f64,
    pins: Vec<DPin>,
}

#[derive(Clone, Debug, Default)]
struct DPin {
    node: String,
    /// 'P' port, 'I' instance pin, 'S' subnode
    kind: char,
    xy: Option<[f64; 2]>,
    /// written as (0, 0): no location
    zero: bool,
    /// `*|I`: instance and its pin
    inst: String,
    pin: String,
}

/// one `*|P`, `*|I` or `*|S` card: node name and point (the last two words when the line has them)
fn dspf_pin(c: &Card) -> Option<DPin> {
    let kind = c.toks[0].chars().nth(2)?;
    let w = &c.toks[1..];
    let n_xy = match kind {
        'P' => 5,
        'I' => 7,
        _ => 3,
    };
    let mut p = DPin { node: w.first()?.clone(), kind, ..Default::default() };
    if kind == 'I' {
        p.inst = w.get(1).cloned().unwrap_or_default();
        p.pin = w.get(2).cloned().unwrap_or_default();
    }
    if w.len() >= n_xy {
        let (x, y) = (si(&w[w.len() - 2]), si(&w[w.len() - 1]));
        if x.is_finite() && y.is_finite() {
            if x == 0.0 && y == 0.0 {
                p.zero = true;
            } else {
                p.xy = Some([x, y]);
            }
        }
    }
    Some(p)
}

fn tail_xy(tail: &[(String, String)], kx: &str, ky: &str) -> Option<[f64; 2]> {
    let g = |k: &str| tail.iter().find(|(a, _)| a == k).map(|(_, v)| si(v)).filter(|v| v.is_finite());
    Some([g(kx)?, g(ky)?])
}

fn is_number(s: &str) -> bool {
    si(s).is_finite()
}

/// layer tag of a whole element / first side, and of the second side
fn tail_tags(tail: &[(String, String)]) -> (Option<String>, Option<String>) {
    let get = |keys: &[&str]| -> Option<String> {
        for k in keys {
            if let Some((key, v)) = tail.iter().find(|(a, _)| a == k) {
                if v.is_empty() {
                    continue;
                }
                // `$l=` is a length when numeric, a layer name otherwise
                if (key == "l" || key == "l1" || key == "l2") && is_number(v) {
                    continue;
                }
                if key.starts_with("lvl") {
                    return Some(format!("lvl{}", v));
                }
                return Some(v.clone());
            }
        }
        None
    };
    (get(&["layer", "lvl", "l", "layer1", "lvl1", "l1"]), get(&["layer2", "lvl2", "l2"]))
}

fn parse_elem(c: &Card, warnings: &mut Vec<String>) -> Option<Result<Elem, ()>> {
    let head = c.toks[0].clone();
    let mut pos: Vec<String> = vec![];
    let mut params: HashMap<String, String> = HashMap::new();
    for t in &c.toks[1..] {
        match t.find('=') {
            Some(k) if k > 0 => {
                params.insert(t[..k].to_lowercase(), t[k + 1..].to_string());
            }
            _ => pos.push(t.clone()),
        }
    }
    let at = |w: &mut Vec<String>, msg: &str| {
        if w.len() < 200 {
            w.push(format!("{} line {}: {} {}", c.file, c.line, head, msg));
        }
    };
    let value = |pos: &[String], params: &HashMap<String, String>, keys: &[&str]| -> f64 {
        if let Some(v) = pos.get(2) {
            return si(v);
        }
        for k in keys {
            if let Some(v) = params.get(*k) {
                return si(v.trim_matches('\''));
            }
        }
        f64::NAN
    };
    let (tag_a, tag_b) = tail_tags(&c.tail);
    match head.chars().next().map(|ch| ch.to_ascii_uppercase()) {
        Some('M') => {
            if pos.len() < 5 {
                at(warnings, "needs d g s b model, not read");
                return Some(Err(()));
            }
            Some(Ok(Elem::Mos { name: head.clone(), nodes: [pos[0].clone(), pos[1].clone(), pos[2].clone(), pos[3].clone()], model: pos[4].clone(), params, xy: tail_xy(&c.tail, "x", "y") }))
        }
        Some('R') => {
            if pos.len() < 2 {
                at(warnings, "needs two nodes, not read");
                return Some(Err(()));
            }
            let ohms = value(&pos, &params, &["r", "resistance", "value"]);
            if !ohms.is_finite() {
                at(warnings, "has no numeric value, kept as NaN");
            }
            Some(Ok(Elem::Res { name: head.clone(), a: pos[0].clone(), b: pos[1].clone(), ohms, tag: tag_a, xy: tail_xy(&c.tail, "x", "y"), xy2: tail_xy(&c.tail, "x2", "y2") }))
        }
        Some('C') => {
            if pos.len() < 2 {
                at(warnings, "needs two nodes, not read");
                return Some(Err(()));
            }
            let farads = value(&pos, &params, &["c", "capacitance", "value"]);
            if !farads.is_finite() {
                at(warnings, "has no numeric value, kept as NaN");
            }
            Some(Ok(Elem::Cap { name: head.clone(), a: pos[0].clone(), b: pos[1].clone(), farads, tag_a, tag_b, xy: tail_xy(&c.tail, "x", "y"), xy2: tail_xy(&c.tail, "x2", "y2") }))
        }
        Some('X') => {
            let (nodes, sub) = match c.toks.iter().position(|t| t == "/") {
                Some(sl) => (c.toks[1..sl].iter().filter(|t| !t.contains('=')).cloned().collect::<Vec<_>>(), c.toks.get(sl + 1).cloned().unwrap_or_default()),
                None => (pos[..pos.len().saturating_sub(1)].to_vec(), pos.last().cloned().unwrap_or_default()),
            };
            if sub.is_empty() {
                at(warnings, "has no subckt name, not read");
                return Some(Err(()));
            }
            // a PDK transistor written as a subckt call (Magic ext2spice for sky130A: X0 d g s b sky130_fd_pr__nfet_01v8 w= l=)
            let sl = sub.to_lowercase();
            if nodes.len() == 4 && params.contains_key("w") && params.contains_key("l") && ["fet", "mos", "nch", "pch"].iter().any(|k| sl.contains(k)) {
                return Some(Ok(Elem::Mos { name: head.clone(), nodes: [nodes[0].clone(), nodes[1].clone(), nodes[2].clone(), nodes[3].clone()], model: sub, params, xy: tail_xy(&c.tail, "x", "y") }));
            }
            Some(Ok(Elem::Inst { name: head.clone(), nodes, sub }))
        }
        _ => None,
    }
}

fn build_subs(cards: &[Card], warnings: &mut Vec<String>) -> Vec<RawSub> {
    let mut done: Vec<RawSub> = vec![];
    let mut stack: Vec<RawSub> = vec![RawSub::default()]; // [0]: elements outside any .subckt
    for c in cards {
        let h = c.toks[0].to_lowercase();
        if h == ".subckt" || h == ".macro" {
            let name = c.toks.get(1).cloned().unwrap_or_default();
            let ports = c.toks.iter().skip(2).filter(|t| !t.contains('=') && !t.eq_ignore_ascii_case("params:")).cloned().collect();
            stack.push(RawSub { name, ports, ..Default::default() });
            continue;
        }
        if h == ".ends" || h == ".eom" {
            if stack.len() > 1 {
                done.push(stack.pop().unwrap());
            } else if warnings.len() < 200 {
                warnings.push(format!("{} line {}: .ends without .subckt", c.file, c.line));
            }
            continue;
        }
        if h.starts_with('.') {
            continue;
        }
        let cur = stack.last_mut().unwrap();
        if let Some(k) = h.strip_prefix("*|") {
            match k {
                "net" => cur.dnets.push(DNet { name: c.toks.get(1).cloned().unwrap_or_default(), total: c.toks.get(2).map(|v| si(v)).unwrap_or(f64::NAN), pins: vec![] }),
                "p" | "i" | "s" => match dspf_pin(c) {
                    Some(p) => {
                        if cur.dnets.is_empty() {
                            cur.dnets.push(DNet::default());
                        }
                        cur.dnets.last_mut().unwrap().pins.push(p);
                    }
                    None => {
                        if warnings.len() < 200 {
                            warnings.push(format!("{} line {}: {} without a node, not read", c.file, c.line, c.toks[0]));
                        }
                    }
                },
                "ground_net" => cur.ground = c.toks.get(1).cloned(),
                _ => {}
            }
            continue;
        }
        match parse_elem(c, warnings) {
            Some(Ok(e)) => cur.elems.push(e),
            Some(Err(())) => {}
            None => cur.other += 1,
        }
    }
    while stack.len() > 1 {
        let s = stack.pop().unwrap();
        warnings.push(format!(".subckt {} has no .ends", s.name));
        done.push(s);
    }
    let top = stack.pop().unwrap();
    if !top.elems.is_empty() {
        done.push(top);
    }
    done
}

// ---------------------------------------------------------------- flattening

#[derive(Default)]
struct Flat {
    names: Vec<String>,
    by_name: HashMap<String, usize>,
    /// (instance path, local name) -> node, for nodes inside instances
    by_path: HashMap<(String, String), usize>,
    mos: Vec<(String, [usize; 4], String, HashMap<String, String>, Option<[f64; 2]>)>,
    res: Vec<(String, String, usize, usize, f64, Option<String>, Option<[f64; 2]>, Option<[f64; 2]>)>,
    caps: Vec<(String, String, usize, Option<usize>, f64, Option<String>, Option<String>, Option<[f64; 2]>, Option<[f64; 2]>)>,
    /// PM instances: (net name, nodes)
    groups: Vec<(String, Vec<usize>)>,
    warnings: Vec<String>,
}

impl Flat {
    fn top_node(&mut self, n: &str) -> usize {
        if let Some(&i) = self.by_name.get(n) {
            return i;
        }
        self.names.push(n.to_string());
        self.by_name.insert(n.to_string(), self.names.len() - 1);
        self.names.len() - 1
    }
    fn inner_node(&mut self, path: &str, local: &str, pretty: &str) -> usize {
        if let Some(&i) = self.by_path.get(&(path.to_string(), local.to_string())) {
            return i;
        }
        let full = format!("{}/{}", path, local);
        let name = if !self.by_name.contains_key(pretty) { pretty.to_string() } else { full };
        let i = self.top_node(&name);
        self.by_path.insert((path.to_string(), local.to_string()), i);
        i
    }
}

fn find_sub<'a>(subs: &'a [RawSub], name: &str) -> Option<&'a RawSub> {
    subs.iter().find(|s| s.name == name).or_else(|| subs.iter().find(|s| s.name.eq_ignore_ascii_case(name)))
}

fn pm_net(sub: &str) -> Option<String> {
    let k = sub.rfind('%')?;
    let n = &sub[k + 1..];
    if n.is_empty() { None } else { Some(n.to_string()) }
}

/// map: local name -> global node (ports), path: instance path ("" at the top), pm: PM net of this instance
fn walk(subs: &[RawSub], sc: &RawSub, map: &HashMap<String, usize>, path: &str, pm: Option<&str>, depth: usize, f: &mut Flat, members: &mut Vec<usize>) -> Result<(), String> {
    if depth > 32 {
        return Err(format!("subckt hierarchy deeper than 32 at {}", sc.name));
    }
    let node = |f: &mut Flat, n: &str, members: &mut Vec<usize>| -> usize {
        let i = if let Some(&i) = map.get(n) {
            i
        } else if path.is_empty() || n.ends_with('!') {
            f.top_node(n)
        } else {
            let pretty = match pm {
                Some(net) => format!("{}:{}", net, n),
                None => format!("{}/{}", path, n),
            };
            f.inner_node(path, n, &pretty)
        };
        if pm.is_some() {
            members.push(i);
        }
        i
    };
    let ename = |n: &str| if path.is_empty() { n.to_string() } else { format!("{}/{}", path, n) };
    for e in &sc.elems {
        match e {
            Elem::Mos { name, nodes, model, params, xy } => {
                let ids = [node(f, &nodes[0], members), node(f, &nodes[1], members), node(f, &nodes[2], members), node(f, &nodes[3], members)];
                f.mos.push((ename(name), ids, model.clone(), params.clone(), *xy));
            }
            Elem::Res { name, a, b, ohms, tag, xy, xy2 } => {
                let (ia, ib) = (node(f, a, members), node(f, b, members));
                f.res.push((name.clone(), ename(name), ia, ib, *ohms, tag.clone(), *xy, *xy2));
            }
            Elem::Cap { name, a, b, farads, tag_a, tag_b, xy, xy2 } => {
                let ga = a == "0";
                let gb = b == "0";
                if ga && gb {
                    continue;
                }
                // keep the ground side second
                let (a, b, xy, xy2, ta, tb) = if ga { (b, a, *xy2, *xy, tag_b.clone(), tag_a.clone()) } else { (a, b, *xy, *xy2, tag_a.clone(), tag_b.clone()) };
                let ia = node(f, a, members);
                let ib = if ga || gb { None } else { Some(node(f, b, members)) };
                let (xy, xy2) = if ga { (xy.or(xy2), None) } else { (xy, xy2) };
                f.caps.push((name.clone(), ename(name), ia, ib, *farads, ta, tb, xy, xy2));
            }
            Elem::Inst { name, nodes, sub } => {
                let s = match find_sub(subs, sub) {
                    Some(s) => s,
                    None => {
                        if f.warnings.len() < 200 {
                            f.warnings.push(format!("instance {} of {}: subckt not defined, not read", ename(name), sub));
                        }
                        continue;
                    }
                };
                if s.ports.len() != nodes.len() {
                    f.warnings.push(format!("instance {} of {}: {} nodes for {} ports", ename(name), sub, nodes.len(), s.ports.len()));
                }
                let mut m = HashMap::new();
                let mut inner_members = vec![];
                for (i, p) in s.ports.iter().enumerate() {
                    if let Some(n) = nodes.get(i) {
                        let id = node(f, n, members);
                        inner_members.push(id);
                        m.insert(p.clone(), id);
                    }
                }
                let ipath = ename(name);
                let inet = pm_net(&s.name);
                if inet.is_some() {
                    walk(subs, s, &m, &ipath, inet.as_deref(), depth + 1, f, &mut inner_members)?;
                    f.groups.push((inet.unwrap(), inner_members));
                } else {
                    walk(subs, s, &m, &ipath, None, depth + 1, f, members)?;
                }
            }
        }
    }
    Ok(())
}

fn mos_count(subs: &[RawSub], s: &RawSub, depth: usize) -> usize {
    if depth > 32 {
        return 0;
    }
    let mut n = 0;
    for e in &s.elems {
        match e {
            Elem::Mos { .. } => n += 1,
            Elem::Inst { sub, .. } => {
                if let Some(c) = find_sub(subs, sub) {
                    n += mos_count(subs, c, depth + 1);
                }
            }
            _ => {}
        }
    }
    n
}

fn find(p: &mut [usize], mut a: usize) -> usize {
    while p[a] != a {
        p[a] = p[p[a]];
        a = p[a];
    }
    a
}
fn unite(p: &mut [usize], a: usize, b: usize) {
    let (a, b) = (find(p, a), find(p, b));
    if a != b {
        p[a.max(b)] = a.min(b);
    }
}

/// MOSFET type from the model name, same rule as `cdl::parse_cdl`
fn mos_type(model: &str) -> &'static str {
    let ml = model.to_lowercase();
    let has_n = ml.contains("nmos") || ml.contains("nfet") || ml.contains("nch");
    if ml.contains('p') && !has_n { "pmos" } else { "nmos" }
}

/// `VSS__2` -> `VSS`
fn alias_base(p: &str) -> Option<&str> {
    let k = p.rfind("__")?;
    let tail = &p[k + 2..];
    if k > 0 && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) { Some(&p[..k]) } else { None }
}

/// Magic `VGND_uq0` -> `VGND`
fn magic_uq_base(p: &str) -> Option<&str> {
    let k = p.rfind("_uq")?;
    let tail = &p[k + 3..];
    if k > 0 && !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) { Some(&p[..k]) } else { None }
}

/// Magic: extresist splits a node `a_361_47#` into `a_361_47.t0`, `.t1`, .. (`killnode` in <cell>.res.ext), but
/// ext2spice still names the old node on a device resistor line (`R0 X a_361_47# sky130_fd_pr__res_generic_m5 ..`).
/// Such a name is replaced by the split node nearest to the line's other nodes. Returns the text and notes.
fn magic_killed_names(text: &str, dir: &Path, load: &mut dyn FnMut(&Path) -> Result<String, String>) -> (Option<String>, Vec<String>) {
    if !text.starts_with("* NGSPICE file created from ") {
        return (None, vec![]);
    }
    let mut killed: HashSet<String> = HashSet::new();
    let mut pts: HashMap<String, [f64; 2]> = HashMap::new();
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() >= 2 && w[0].eq_ignore_ascii_case(".subckt") {
            let Ok(t) = load(&dir.join(format!("{}.res.ext", w[1]))) else { continue };
            for r in t.lines() {
                let v: Vec<&str> = r.split_whitespace().collect();
                match v.first().copied() {
                    Some("killnode") if v.len() >= 2 => {
                        killed.insert(v[1].trim_matches('"').to_string());
                    }
                    Some("rnode") if v.len() >= 6 => {
                        if let (Ok(x), Ok(y)) = (v[4].parse::<f64>(), v[5].parse::<f64>()) {
                            pts.insert(v[1].trim_matches('"').to_string(), [x, y]);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    if killed.is_empty() {
        return (None, vec![]);
    }
    let mut notes = vec![];
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.iter().any(|t| killed.contains(*t)) && !w.is_empty() && !w[0].starts_with('*') {
            let others: Vec<[f64; 2]> = w.iter().filter_map(|t| pts.get(*t)).copied().collect();
            let mut nl: Vec<String> = vec![];
            for t in &w {
                if !killed.contains(*t) {
                    nl.push(t.to_string());
                    continue;
                }
                let base = t.trim_end_matches('#');
                let pre = format!("{}.t", base);
                let d = |p: &[f64; 2]| others.iter().map(|q| (p[0] - q[0]).hypot(p[1] - q[1])).fold(f64::INFINITY, f64::min);
                let best = pts.iter().filter(|(n, _)| n.starts_with(&pre) && n[pre.len()..].bytes().all(|b| b.is_ascii_digit())).min_by(|a, b| d(a.1).partial_cmp(&d(b.1)).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(b.0)));
                match best {
                    Some((n, _)) => {
                        notes.push(format!("Magic: {} names {}, a node extresist split (killnode); it is joined to {}, the split node nearest to the line's other nodes", w[0], t, n));
                        nl.push(n.clone());
                        changed = true;
                    }
                    None => nl.push(t.to_string()),
                }
            }
            out.push_str(&nl.join(" "));
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    (if changed { Some(out) } else { None }, notes)
}

fn build_cell(subs: &[RawSub], top: &RawSub) -> Result<LpeCell, String> {
    let mut f = Flat::default();
    let mut map = HashMap::new();
    for p in &top.ports {
        let i = f.top_node(p);
        map.insert(p.clone(), i);
    }
    let mut members = vec![];
    walk(subs, top, &map, "", None, 0, &mut f, &mut members)?;
    // DSPF: the nodes a `*|NET` section lists form that net, like a PM instance
    for d in &top.dnets {
        let ids: Vec<usize> = d.pins.iter().map(|p| f.top_node(&p.node)).collect();
        if !d.name.is_empty() && !ids.is_empty() {
            f.groups.push((d.name.clone(), ids));
        }
    }
    let n = f.names.len();
    let mut parent: Vec<usize> = (0..n).collect();
    let mut other_use = vec![false; n];
    for r in &f.res {
        unite(&mut parent, r.2, r.3);
        other_use[r.2] = true;
        other_use[r.3] = true;
    }
    for c in &f.caps {
        other_use[c.2] = true;
        if let Some(b) = c.3 {
            other_use[b] = true;
        }
    }
    for (_, g) in &f.groups {
        for &x in g {
            other_use[x] = true;
            unite(&mut parent, g[0], x);
        }
    }
    let mut used_dgs = vec![false; n];
    for m in &f.mos {
        for &x in &m.1[..3] {
            used_dgs[x] = true;
        }
    }
    let mut aliases = vec![];
    for p in &top.ports {
        let pi = f.by_name[p];
        // Magic `VGND_uq0`: a second piece of port VGND that the cell does not connect inside; one net, as the
        // CDL and the layout extraction (join-same-name) have it
        if let Some(base) = magic_uq_base(p).filter(|b| top.ports.iter().any(|q| q == b)) {
            let bi = f.by_name[base];
            if bi != pi {
                unite(&mut parent, pi, bi);
                aliases.push((p.clone(), base.to_string()));
            }
            continue;
        }
        if let Some(base) = alias_base(p) {
            if let Some(&bi) = f.by_name.get(base) {
                if !other_use[pi] && !used_dgs[pi] && bi != pi {
                    unite(&mut parent, pi, bi);
                    aliases.push((p.clone(), base.to_string()));
                }
            }
        }
    }
    // nets: components in order of first node
    let mut comp_of: HashMap<usize, usize> = HashMap::new();
    let mut nets: Vec<LpeNet> = vec![];
    let mut node_net = vec![0usize; n];
    for i in 0..n {
        let r = find(&mut parent, i);
        let k = *comp_of.entry(r).or_insert_with(|| {
            nets.push(LpeNet::default());
            nets.len() - 1
        });
        nets[k].nodes.push(i);
        node_net[i] = k;
    }
    let mut warnings = std::mem::take(&mut f.warnings);
    let mut pm_name: Vec<Option<String>> = vec![None; nets.len()];
    for (net, g) in &f.groups {
        if let Some(&x) = g.first() {
            let k = node_net[x];
            match &pm_name[k] {
                None => pm_name[k] = Some(net.clone()),
                Some(o) if o != net => warnings.push(format!("PM nets {} and {} are joined by resistors, named {}", o, net, o)),
                _ => {}
            }
        }
    }
    let mut taken: HashSet<String> = HashSet::new();
    for k in 0..nets.len() {
        let mut name = if let Some(p) = &pm_name[k] {
            nets[k].from_pm = true;
            p.clone()
        } else if let Some(p) = top.ports.iter().find(|p| node_net[f.by_name[*p]] == k && !aliases.iter().any(|a| &a.0 == *p)) {
            p.clone()
        } else {
            f.names[nets[k].nodes[0]].clone()
        };
        if taken.contains(&name) {
            let base = name.clone();
            let mut j = 2;
            while taken.contains(&format!("{}#{}", base, j)) {
                j += 1;
            }
            name = format!("{}#{}", base, j);
            warnings.push(format!("two nets would be called {}, the second is {}", base, name));
        }
        taken.insert(name.clone());
        nets[k].name = name;
    }
    // ports that stay separate nets but share a PM net name with another port
    let mut ports: Vec<String> = vec![];
    for p in &top.ports {
        let nm = nets[node_net[f.by_name[p]]].name.clone();
        if !ports.contains(&nm) {
            ports.push(nm);
        }
    }
    let mut nodes: Vec<LpeNode> = f.names.iter().enumerate().map(|(i, nm)| LpeNode { name: nm.clone(), net: node_net[i], xy: None, term: vec![] }).collect();
    // DSPF: a node's location is its `*|P` / `*|I` / `*|S` point, (0, 0) there means none
    let mut dspf_xy = vec![false; n];
    let (mut zero_pts, mut pin_conf) = (0usize, 0usize);
    for p in top.dnets.iter().flat_map(|d| d.pins.iter()) {
        let i = f.by_name[&p.node];
        zero_pts += p.zero as usize;
        if let Some(q) = p.xy {
            match nodes[i].xy {
                None => {
                    nodes[i].xy = Some(q);
                    dspf_xy[i] = true;
                }
                Some(o) if (o[0] - q[0]).abs() > 1e-6 || (o[1] - q[1]).abs() > 1e-6 => pin_conf += 1,
                _ => {}
            }
        }
    }
    let mut xy_conflicts = 0usize;
    let mut set_xy = |nodes: &mut Vec<LpeNode>, i: usize, xy: Option<[f64; 2]>| {
        if let Some(p) = xy {
            match nodes[i].xy {
                None => nodes[i].xy = Some(p),
                Some(q) if (q[0] - p[0]).abs() > 1e-6 || (q[1] - p[1]).abs() > 1e-6 => xy_conflicts += 1,
                _ => {}
            }
        }
    };
    let mut sc = Subckt { name: top.name.clone(), ports, ..Default::default() };
    let mut devices = vec![];
    // DSPF: a device named like its `*|I` instance plus one leading letter (MMM0 for MM0) takes the instance name
    let pin_inst: HashMap<usize, &str> = top.dnets.iter().flat_map(|d| d.pins.iter()).filter(|p| p.kind == 'I').map(|p| (f.by_name[&p.node], p.inst.as_str())).collect();
    let mut renamed = 0usize;
    for (di, (name, ids, model, params, xy)) in f.mos.iter().enumerate() {
        let name = match ids.iter().filter_map(|i| pin_inst.get(i)).find(|inst| name.len() == inst.len() + 1 && name[1..].eq_ignore_ascii_case(inst)) {
            Some(inst) => {
                renamed += 1;
                inst.to_string()
            }
            None => name.clone(),
        };
        let name = &name;
        let get = |k: &str, d: f64| params.get(k).map(|v| si(v)).unwrap_or(d);
        let nn = |i: usize| nets[node_net[i]].name.clone();
        sc.mos.push(Mos { name: name.clone(), typ: mos_type(model).into(), model: model.clone(), d: nn(ids[0]), g: nn(ids[1]), s: nn(ids[2]), b: nn(ids[3]), w: get("w", f64::NAN), l: get("l", f64::NAN), nfin: get("nfin", f64::NAN), m: get("m", 1.0), nf: get("nf", 1.0) });
        for (k, t) in ['d', 'g', 's', 'b'].iter().enumerate() {
            nodes[ids[k]].term.push((di, *t));
        }
        devices.push(LpeDevice { name: name.clone(), nodes: *ids, xy: *xy });
    }
    // terminal from the name N_<net>_<device>_<d|g|s|b> for nodes not on a device line
    let mut by_dev: HashMap<&str, usize> = HashMap::new();
    for (i, d) in devices.iter().enumerate() {
        by_dev.insert(d.name.as_str(), i);
    }
    for nd in nodes.iter_mut() {
        if !nd.term.is_empty() {
            continue;
        }
        let s = match nd.name.strip_prefix("N_") {
            Some(s) => s,
            None => continue,
        };
        let (rest, t) = match s.rsplit_once('_') {
            Some((r, t)) if t.len() == 1 && "dgsb".contains(t) => (r, t.chars().next().unwrap()),
            _ => continue,
        };
        let hit = by_dev.iter().filter(|(d, _)| rest.ends_with(&format!("_{}", d))).max_by_key(|(d, _)| d.len());
        if let Some((_, dev)) = hit {
            nd.term.push((*dev, t));
        }
    }
    // DSPF `*|I (<pin node> <instance> <pin> ..)` for a node no device line names
    for p in top.dnets.iter().flat_map(|d| d.pins.iter()).filter(|p| p.kind == 'I') {
        let i = f.by_name[&p.node];
        let t = p.pin.to_lowercase().chars().next().filter(|c| "dgsb".contains(*c));
        let dev = by_dev.get(p.inst.as_str()).or_else(|| by_dev.iter().find(|(d, _)| d.eq_ignore_ascii_case(&p.inst)).map(|(_, v)| v));
        if let (true, Some(t), Some(&dev)) = (nodes[i].term.is_empty(), t, dev) {
            nodes[i].term.push((dev, t));
        }
    }
    let mut res = vec![];
    for (_, name, a, b, ohms, tag, xy, xy2) in &f.res {
        set_xy(&mut nodes, *a, *xy);
        set_xy(&mut nodes, *b, *xy2);
        res.push(LpeRes { name: name.clone(), a: *a, b: *b, ohms: *ohms, net: node_net[*a], tag: tag.clone() });
    }
    // DSPF: a coupling C second point ($X2 $Y2) other than its node's `*|` point is wrong in some extractor output, use the node point
    let (mut fixed, mut zero_el) = (0usize, 0usize);
    let mut node_pt = |nodes: &Vec<LpeNode>, i: usize, xy: Option<[f64; 2]>, second: bool| -> Option<[f64; 2]> {
        if top.dnets.is_empty() {
            return xy;
        }
        let differs = |p: [f64; 2]| nodes[i].xy.map_or(false, |q| (q[0] - p[0]).abs() > 1e-6 || (q[1] - p[1]).abs() > 1e-6);
        match (xy, dspf_xy[i]) {
            (Some(p), true) if differs(p) && (second || p == [0.0, 0.0]) => {
                fixed += 1;
                nodes[i].xy
            }
            (Some(p), false) if p == [0.0, 0.0] => {
                zero_el += 1;
                None
            }
            _ => xy,
        }
    };
    let mut caps = vec![];
    for (_, name, a, b, farads, ta, tb, xy, xy2) in &f.caps {
        let xy = node_pt(&nodes, *a, *xy, false);
        let xy2 = match b {
            Some(b) => node_pt(&nodes, *b, *xy2, true),
            None => None,
        };
        set_xy(&mut nodes, *a, xy);
        if let Some(b) = b {
            set_xy(&mut nodes, *b, xy2);
        }
        caps.push(LpeCap { name: name.clone(), a: *a, b: *b, farads: *farads, xy, xy2, net_a: node_net[*a], net_b: b.map(|b| node_net[b]), coupling: b.is_some(), tag_a: ta.clone(), tag_b: tb.clone() });
    }
    if xy_conflicts > 0 {
        warnings.push(format!("{} node coordinates differ between lines; the first one is kept", xy_conflicts));
    }
    let mut notes = vec![];
    if !top.dnets.is_empty() {
        notes.push(format!("DSPF: {} *|NET sections, {} *|P/*|I/*|S lines", top.dnets.len(), top.dnets.iter().map(|d| d.pins.len()).sum::<usize>()));
        if renamed > 0 {
            notes.push(format!("DSPF: {} devices take the instance name of their *|I lines (the device line adds a letter, e.g. MMM0 for MM0)", renamed));
        }
        if zero_pts > 0 {
            notes.push(format!("DSPF: {} *|P/*|I/*|S points are (0, 0), read as no location (some extractors write 0 0 for a node they have no location for)", zero_pts));
        }
        if pin_conf > 0 {
            warnings.push(format!("DSPF: {} nodes are listed again with another point, the first one is kept", pin_conf));
        }
        if fixed > 0 {
            notes.push(format!("DSPF: {} coupling C second points ($X2 $Y2) differ from their node's *|P/*|I/*|S point, the node point is used (some extractors write wrong $X2 $Y2)", fixed));
        }
        if zero_el > 0 {
            notes.push(format!("DSPF: {} capacitor sides at (0, 0) on a node without a point, read as no location", zero_el));
        }
        // the `*|NET` total is the net's ground C plus every coupling C touching it
        let (mut same, mut checked) = (0usize, 0usize);
        for d in top.dnets.iter().filter(|d| !d.name.is_empty() && d.total.is_finite()) {
            let Some(k) = nets.iter().position(|x| x.name == d.name) else { continue };
            checked += 1;
            let s: f64 = caps.iter().filter(|c| c.net_a == k || c.net_b == Some(k)).map(|c| c.farads).sum();
            if (s - d.total).abs() <= 1e-3 * d.total.abs() + 1e-24 {
                same += 1;
            } else {
                warnings.push(format!("DSPF *|NET {}: total {:.6} fF, its C and Cc add up to {:.6} fF", d.name, d.total * 1e15, s * 1e15));
            }
        }
        notes.push(format!("DSPF: *|NET total C equals the sum of the net's C and Cc for {} of {} nets", same, checked));
    }
    // short element names unless two flattened elements share one
    let mut count: HashMap<String, usize> = HashMap::new();
    for r in &f.res {
        *count.entry(r.0.clone()).or_insert(0) += 1;
    }
    for c in &f.caps {
        *count.entry(c.0.clone()).or_insert(0) += 1;
    }
    for (r, raw) in res.iter_mut().zip(&f.res) {
        if count[&raw.0] == 1 {
            r.name = raw.0.clone();
        }
    }
    for (c, raw) in caps.iter_mut().zip(&f.caps) {
        if count[&raw.0] == 1 {
            c.name = raw.0.clone();
        }
    }
    sc.other = res.len() + caps.len();
    if sc.mos.is_empty() {
        warnings.push("no MOSFETs".into());
    }
    Ok(LpeCell { name: top.name.clone(), ports: top.ports.clone(), subckt: sc, nodes, nets, res, caps, devices, aliases, warnings, notes })
}

/// Parse LPE SPICE text. `load` reads an included file (the path is already resolved).
pub fn parse_lpe(text: &str, path: &Path, load: &mut dyn FnMut(&Path) -> Result<String, String>) -> Result<LpeFile, String> {
    let mut rd = Reader { main_dir: path.parent().map(|d| d.to_path_buf()).unwrap_or_default(), files: vec![path.to_path_buf()], warnings: vec![], load };
    let (fixed_text, magic_notes) = magic_killed_names(text, &rd.main_dir.clone(), rd.load);
    let text = fixed_text.as_deref().unwrap_or(text);
    let mut cards = vec![];
    rd.read(path, text, 0, &mut cards)?;
    let mut warnings = std::mem::take(&mut rd.warnings);
    warnings.extend(magic_notes);
    let mut subs = build_subs(&cards, &mut warnings);
    // DSPF `*LAYER_MAP`: `$lvl=<n>` tags become layer names
    let lmap: HashMap<String, String> = cards.iter().filter(|c| c.toks[0] == "*|LAYERMAP" && c.toks.len() >= 3).map(|c| (format!("lvl{}", c.toks[1]), c.toks[2].clone())).collect();
    let dspf = cards.iter().any(|c| c.toks[0].starts_with("*|"));
    for s in subs.iter_mut() {
        if !lmap.is_empty() {
            let map = |t: &mut Option<String>| {
                if let Some(n) = t.as_ref().and_then(|v| lmap.get(v)) {
                    *t = Some(n.clone());
                }
            };
            for e in s.elems.iter_mut() {
                match e {
                    Elem::Res { tag, .. } => map(tag),
                    Elem::Cap { tag_a, tag_b, .. } => {
                        map(tag_a);
                        map(tag_b);
                    }
                    _ => {}
                }
            }
        }
        // a DSPF ground net that is not a port: capacitors to it are ground capacitors
        if let Some(g) = s.ground.clone().filter(|g| g != "0" && !s.ports.contains(g)) {
            for e in s.elems.iter_mut() {
                if let Elem::Cap { a, b, .. } = e {
                    for n in [a, b] {
                        if *n == g {
                            *n = "0".into();
                        }
                    }
                }
            }
        }
    }
    let used: HashSet<String> = subs.iter().flat_map(|s| s.elems.iter()).filter_map(|e| if let Elem::Inst { sub, .. } = e { Some(sub.to_lowercase()) } else { None }).collect();
    let mut cells = vec![];
    for s in &subs {
        if used.contains(&s.name.to_lowercase()) || mos_count(&subs, s, 0) == 0 {
            continue;
        }
        let mut c = build_cell(&subs, s)?;
        if c.name.is_empty() {
            c.name = path.file_name().map(|b| b.to_string_lossy().to_string()).unwrap_or_default();
            c.subckt.name = c.name.clone();
        }
        cells.push(c);
    }
    if cells.is_empty() {
        let why = if dspf { " (a DSPF written without its Instance Section has no devices)" } else { "" };
        return Err(format!("{}: no subckt with MOSFETs that is not instantiated by another subckt{}", path.display(), why));
    }
    // Magic (ext2spice with extresist on): the node points are in <cell>.res.ext next to the SPICE file
    let mut files = rd.files;
    for c in cells.iter_mut() {
        let rp = rd.main_dir.join(format!("{}.res.ext", c.name));
        let Ok(t) = (rd.load)(&rp) else { continue };
        match magic_points(&t) {
            Ok((um, pts)) => {
                let (mut set, mut had, mut unknown) = (0usize, 0usize, 0usize);
                for (name, p) in &pts {
                    match c.node_index(name) {
                        Some(i) if c.nodes[i].xy.is_none() => {
                            c.nodes[i].xy = Some(*p);
                            set += 1;
                        }
                        Some(_) => had += 1,
                        None => unknown += 1,
                    }
                }
                let file = rp.file_name().map(|b| b.to_string_lossy().to_string()).unwrap_or_default();
                c.notes.push(format!("Magic: {} of {} nodes take their point from the {} rnode lines of {} (1 unit = {} nm)", set, c.nodes.len(), pts.len(), file, um * 1e3));
                if had > 0 {
                    c.notes.push(format!("Magic: {} rnode lines name a node that already has a point from the SPICE file, that point is kept", had));
                }
                if unknown > 0 {
                    c.warnings.push(format!("Magic: {} rnode lines of {} name no node of the SPICE file", unknown, file));
                }
                files.push(rp);
                // the substrate node of <cell>.ext (`substrate "VNB" ...`): a C to it is a C to ground
                let ep = rd.main_dir.join(format!("{}.ext", c.name));
                let sub = (rd.load)(&ep).ok().and_then(|t| t.lines().find_map(|l| l.strip_prefix("substrate ").and_then(|r| r.split_whitespace().next()).map(|s| s.trim_matches('"').to_string())));
                if let Some(k) = sub.as_deref().and_then(|s| c.net_index(s)) {
                    let mut n = 0;
                    for cap in c.caps.iter_mut().filter(|x| x.coupling) {
                        if cap.net_a == k && cap.net_b != Some(k) {
                            cap.a = cap.b.unwrap();
                            cap.net_a = cap.net_b.unwrap();
                            cap.xy = cap.xy2;
                            cap.tag_a = cap.tag_b.take();
                        } else if cap.net_b != Some(k) {
                            continue;
                        }
                        cap.b = None;
                        cap.net_b = None;
                        cap.xy2 = None;
                        cap.tag_b = None;
                        cap.coupling = false;
                        n += 1;
                    }
                    c.notes.push(format!("Magic: {} C to the substrate node {} ({}) are read as C to ground", n, c.nets[k].name, ep.file_name().map(|b| b.to_string_lossy().to_string()).unwrap_or_default()));
                    files.push(ep);
                }
            }
            Err(e) => c.warnings.push(format!("{}: {}", rp.display(), e)),
        }
    }
    Ok(LpeFile { cells, files, warnings })
}

/// The `rnode` points of a Magic `.res.ext` file (written by `extresist`), in µm, and µm per Magic unit.
/// `scale <r> <c> <l>`: l is the length unit in 1e-8 µm (sky130A: 500000, i.e. 5 nm, which is how ext2spice turns a
/// 130-unit device edge into w=0.65). `rnode "<name>" 0 <C aF> <x> <y> 0`.
pub fn magic_points(text: &str) -> Result<(f64, Vec<(String, [f64; 2])>), String> {
    let mut um = None;
    let mut pts = vec![];
    for (ln, line) in text.lines().enumerate() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.first().copied() {
            Some("scale") if w.len() >= 4 => {
                let l: f64 = w[3].parse().map_err(|_| format!("line {}: scale length unit {} is not a number", ln + 1, w[3]))?;
                um = Some(l * 1e-8);
            }
            Some("rnode") if w.len() >= 6 => {
                let u = um.ok_or_else(|| format!("line {}: rnode before the scale line", ln + 1))?;
                let name = w[1].trim_matches('"').to_string();
                let (x, y): (f64, f64) = match (w[4].parse(), w[5].parse()) {
                    (Ok(x), Ok(y)) => (x, y),
                    _ => return Err(format!("line {}: rnode point {} {} is not two numbers", ln + 1, w[4], w[5])),
                };
                pts.push((name, [x * u, y * u]));
            }
            _ => {}
        }
    }
    let um = um.ok_or("no scale line")?;
    Ok((um, pts))
}

/// Read an LPE SPICE file (gzip allowed) and its includes.
pub fn read_lpe(path: &Path) -> Result<LpeFile, String> {
    let mut load = |p: &Path| -> Result<String, String> { crate::io::read_all(p).map(|b| String::from_utf8_lossy(&b).into_owned()).map_err(|e| format!("{}: {}", p.display(), e)) };
    let text = load(path)?;
    parse_lpe(&text, path, &mut load)
}

// ---------------------------------------------------------------- layer attribution

/// a node point within this distance of a layout shape is on that shape
pub const TOL_NM: f64 = 1.0;
/// second try when no shape is within TOL_NM
pub const NEAR_NM: f64 = 5.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Src {
    #[default]
    Unknown,
    /// `$layer=` style tail tag on the element
    Tag,
    /// a top port node at a GDS pin label of its name: the conductor the label is attached to
    Pin,
    /// exactly one layer of the net's layout shapes holds the point
    Shape,
    /// a device terminal node: gate layer for g, source/drain layer for d and s
    Terminal,
    /// several layers hold the point (a via or contact stack): the one most strongly attributed R
    /// neighbours are on
    Neighbors,
    /// several layers hold the point and no strongly attributed R neighbour decides: the top candidate
    /// is picked (a convention, not evidence, `candidates` lists the stack)
    Stack,
    /// no point: the layer of the attributed nodes reached over resistors
    Inferred,
}

#[derive(Clone, Debug, Default)]
pub struct NodeLayer {
    pub layer: Option<String>,
    pub src: Src,
    /// node layers of the net's layout shapes at the point, lowest first
    pub candidates: Vec<String>,
    /// cut layers (vias, contacts) of the net at the point
    pub cuts: Vec<String>,
    /// the point used, µm, and where it came from ("c12", "cc_3", "pin label")
    pub xy: Option<[f64; 2]>,
    pub xy_from: String,
    /// how the layer was chosen, or why there is none
    pub how: String,
}

#[derive(Clone, Debug, Default)]
pub struct ResLayer {
    /// the layer for a resistor inside one layer, the joining via or contact chain otherwise
    pub layer: Option<String>,
    /// the layer used for each end (an end without its own layer is inferred over the other resistors)
    pub ends: [Option<String>; 2],
    pub ends_src: [Src; 2],
    pub how: String,
    /// the layer comes from the R value, equal to R placed by position in the same cell (not from its own ends)
    pub by_value: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CapLayer {
    pub a: Option<String>,
    /// second side of a coupling cap (None for a ground cap)
    pub b: Option<String>,
    pub how: String,
}

#[derive(Clone, Debug, Default)]
pub struct Attribution {
    pub nodes: Vec<NodeLayer>,
    pub res: Vec<ResLayer>,
    pub caps: Vec<CapLayer>,
    /// layers a node can sit on and cut layers (vias, contacts), lowest first
    pub node_layers: Vec<String>,
    pub cut_layers: Vec<String>,
    /// layout nets used for each LPE net (the LVS map unless the node points say otherwise)
    pub net_layout: Vec<Vec<usize>>,
    pub notes: Vec<String>,
    /// the source/drain layer of the device rule
    pub sd_layer: Option<String>,
}

fn is_cut_layer(name: &str) -> bool {
    let l = name.to_lowercase();
    let num_after = |p: &str| l.strip_prefix(p).map_or(false, |r| !r.is_empty() && r.bytes().all(|b| b.is_ascii_digit()));
    num_after("v") || l.contains("via") || ["sdc", "co", "cont", "contact", "ct", "mcon", "licon", "licon1", "cut"].contains(&l.as_str())
}

fn is_bulk_layer(name: &str) -> bool {
    ["well", "nwell", "pwell", "sub", "substrate", "psub", "nsub", "tap"].contains(&name.to_lowercase().as_str())
}

struct Stack {
    adj: HashMap<String, Vec<String>>,
    height: HashMap<String, usize>,
}

impl Stack {
    fn h(&self, l: &str) -> usize {
        self.height.get(l).copied().unwrap_or(usize::MAX / 2)
    }
    fn path(&self, a: &str, b: &str) -> Option<Vec<String>> {
        let mut prev: HashMap<String, String> = HashMap::new();
        let mut q = VecDeque::from([a.to_string()]);
        let mut seen: HashSet<String> = HashSet::from([a.to_string()]);
        while let Some(x) = q.pop_front() {
            if x == b {
                let mut p = vec![x.clone()];
                let mut c = x;
                while let Some(pr) = prev.get(&c) {
                    p.push(pr.clone());
                    c = pr.clone();
                }
                p.reverse();
                return Some(p);
            }
            let mut next = self.adj.get(&x).cloned().unwrap_or_default();
            next.sort_by_key(|n| (self.h(n), n.clone()));
            for n in next {
                if seen.insert(n.clone()) {
                    prev.insert(n.clone(), x.clone());
                    q.push_back(n);
                }
            }
        }
        None
    }
    /// "V0 (LISD-M1)", "gate-LIG", "diff-M1 (sdc, LISD, V0)"
    fn join(&self, a: &str, b: &str) -> String {
        let (lo, hi) = if (self.h(a), a) <= (self.h(b), b) { (a, b) } else { (b, a) };
        match self.path(lo, hi) {
            None => format!("{}-{} (no connection in the rules)", lo, hi),
            Some(p) if p.len() <= 2 => format!("{}-{}", lo, hi),
            Some(p) if p.len() == 3 => format!("{} ({}-{})", p[1], lo, hi),
            Some(p) => format!("{}-{} ({})", lo, hi, p[1..p.len() - 1].join(", ")),
        }
    }
}

fn label_layer_key(src: &str, ex: &Extraction) -> Option<(i32, i32)> {
    let mut it = src.splitn(2, '/');
    if let (Some(a), Some(b)) = (it.next(), it.next()) {
        if let (Ok(l), Ok(d)) = (a.parse(), b.parse()) {
            return Some((l, d));
        }
    }
    ex.prim.iter().find(|p| p.0 == src).map(|p| p.1)
}

/// attributed nodes reached from `start` over resistors (skipping resistor `skip`), through nodes
/// without a layer of their own, each with the layers it may be on
fn boundary(start: usize, skip: Option<usize>, adj: &[Vec<(usize, usize)>], sets: &[Option<Vec<String>>]) -> Vec<(usize, Vec<String>)> {
    let mut seen = HashSet::from([start]);
    let mut q = VecDeque::from([start]);
    let mut found = vec![];
    while let Some(x) = q.pop_front() {
        for &(y, k) in &adj[x] {
            if Some(k) == skip || !seen.insert(y) {
                continue;
            }
            match &sets[y] {
                Some(l) => found.push((y, l.clone())),
                None => q.push_back(y),
            }
        }
    }
    found
}

/// Ok(layer common to every found node) or Err(why)
fn common_layer(found: &[(usize, Vec<String>)], order: &dyn Fn(&String) -> (usize, String)) -> Result<String, String> {
    if found.is_empty() {
        return Err("no attributed node".into());
    }
    let mut common: Vec<String> = found[0].1.clone();
    for (_, s) in &found[1..] {
        common.retain(|l| s.contains(l));
    }
    let mut all: Vec<String> = found.iter().flat_map(|f| f.1.iter().cloned()).collect();
    all.sort_by_key(|l| order(l));
    all.dedup();
    common.sort_by_key(|l| order(l));
    match common.len() {
        1 => Ok(common[0].clone()),
        0 => Err(format!("joins {}", all.join(", "))),
        _ => Err(format!("could be on {}", common.join(" or "))),
    }
}

/// Attribute every node, R and C of `cell` to a layout layer.
///
/// `ex` is the extraction of the same cell's GDS (`flat`), `m` the LVS match of `cell.subckt` against
/// it, `meters_per_db` the GDS database unit (LPE coordinates are µm), `rules_text` the extraction rules
/// (`conduct` and `connect` give the layer stack, `device` the gate and source/drain layers, `label`
/// the pin label layers).
///
/// Layout net of each LPE net: the LVS map (`m.net_to_layout`), unless a unique other layout net holds
/// more of the net's node points (a device terminal point counts only on its terminal layer). The
/// matcher cannot tell identical stages apart and may map them crosswise. `Attribution::net_layout`
/// and `notes` give the nets used.
///
/// Node layer, first rule that applies:
/// 1. point: the node's first `$X $Y` (C, Cc or R line). A top port node without one takes the
///    position of the GDS pin label of its name.
/// 2. candidates: node layers (conductors except cut layers such as V0, V1, sdc and except the well)
///    whose shapes of the node's layout net(s) hold the point within TOL_NM, else within NEAR_NM.
/// 3. a top port node at a pin label of its name: the conductor the rules attach that label to, when it
///    is a candidate (Src::Pin).
/// 4. one candidate: that layer (Src::Shape).
/// 5. a device terminal: the device rule's gate layer for g, source/drain layer for d and s, when it is
///    a candidate or when the node has no point (Src::Terminal).
/// 6. several candidates (a via or contact stack): the candidate that the most R neighbours at other
///    points attributed by rules 3-5 (or a tag) are on (Src::Neighbors), with no chains through other
///    stack decisions. Otherwise the top candidate is picked by convention (Src::Stack), `candidates`
///    keeps the whole stack. A reduced network means a node on the ASAP7 rail stack (M1 over
///    the LIG strip, LISD and V0 at the LISD ends) cannot be placed on one layer from its point alone.
/// 7. no point and not a terminal (internal node of the R network):
///    a. its R neighbours include two device terminals whose points lie in one shape of the gate (or
///       source/drain) layer: the junction is on that layer (poly branching to fingers, diffusion
///       shared by fingers) (Src::Inferred),
///    b. else the layer of the attributed nodes reached first over resistors, passing through other
///       nodes without a layer and through stack picks, when they are all on one layer (Src::Inferred),
///    c. else unknown, naming the layers it joins.
///
/// Elements: a `$layer=` tag wins. A ground C takes its node's layer (the layer at the C's own point
/// if that differs from the node's), a coupling C one layer per side. An R end takes its node's layer
/// from rules 1-6. An end without one uses rule 7a, else 7b over the other resistors only (the R itself
/// is skipped). Two ends at one point of a stack: the vertical connection there, named by the cut
/// layers at the point and the stack, e.g. "V0 (LIG+LISD+M1)". Both ends on one layer: the R is on
/// that layer. Different layers: the via or contact chain joining them in the rules' connect graph,
/// e.g. "V0 (LISD-M1)", "gate-M1 (LIG, V0)", "diff-M1 (sdc, LISD, V0)". An end that stays unknown makes
/// the R unknown, with the reason in `how`. `ResLayer::ends_src` shows which ends rest on a stack pick.
/// Value match: an unknown R whose value equals (within 1e-5) a value shared by at least two R of one class placed
/// by position (both ends by pin, shape, terminal or tag, or both at one point of a stack), and by no attributed R
/// of another class, takes that class with `ResLayer::by_value` set. Each class used is listed in `notes`.
pub fn attribute_layers(cell: &LpeCell, ex: &Extraction, m: &MatchResult, flat: &crate::gds::Flat, meters_per_db: f64, rules_text: &str) -> Attribution {
    let rules = parse_rules(rules_text);
    let mut st = Stack { adj: HashMap::new(), height: HashMap::new() };
    for (a, b) in &rules.connect {
        st.adj.entry(a.clone()).or_default().push(b.clone());
        st.adj.entry(b.clone()).or_default().push(a.clone());
    }
    let gate_layer = rules.devices.first().and_then(|d| d.kv.get("gate").cloned());
    let sd_layer = rules.devices.first().and_then(|d| d.kv.get("sd").cloned());
    {
        let mut q = VecDeque::new();
        for l in gate_layer.iter().chain(sd_layer.iter()) {
            st.height.insert(l.clone(), 0);
            q.push_back(l.clone());
        }
        while let Some(x) = q.pop_front() {
            let h = st.height[&x];
            for n in st.adj.get(&x).cloned().unwrap_or_default() {
                if !st.height.contains_key(&n) {
                    st.height.insert(n.clone(), h + 1);
                    q.push_back(n);
                }
            }
        }
    }
    let mut node_layers: Vec<String> = rules.conduct.iter().filter(|l| !is_cut_layer(l) && !is_bulk_layer(l)).cloned().collect();
    node_layers.sort_by_key(|l| (st.h(l), l.clone()));
    let mut cut_layers: Vec<String> = rules.conduct.iter().filter(|l| is_cut_layer(l)).cloned().collect();
    cut_layers.sort_by_key(|l| (st.h(l), l.clone()));
    let shapes: HashMap<&str, &HashMap<usize, Vec<[i32; 4]>>> = ex.shapes.iter().map(|(l, s)| (l.as_str(), s)).collect();
    // a well net (only MOSFET bulk terminals, e.g. VPB and VNB of sky130): its nodes are on the well layer. Magic writes
    // well resistors with node points at the devices, which lie over other nets' wires.
    let bulk_layers: Vec<String> = rules.conduct.iter().filter(|l| is_bulk_layer(l)).cloned().collect();
    let well_net: Vec<bool> = cell.nets.iter().map(|net| {
        let t: Vec<char> = net.nodes.iter().flat_map(|&i| cell.nodes[i].term.iter().map(|x| x.1)).collect();
        !bulk_layers.is_empty() && t.contains(&'b') && t.iter().all(|&c| c == 'b')
    }).collect();
    let layers_of = |k: usize| -> &Vec<String> { if well_net[k] { &bulk_layers } else { &node_layers } };
    let per_um = 1e-6 / meters_per_db;
    let tol_um = TOL_NM * 1e-3;
    let mut notes = vec![];
    if !meters_per_db.is_finite() || meters_per_db <= 0.0 {
        notes.push(format!("GDS database unit {} m is not usable; no geometry", meters_per_db));
    }
    // LPE net -> layout nets: the LVS map, unless the net's node points lie on another layout net
    // (nets the matcher cannot tell apart, such as four identical stages, may be mapped crosswise)
    let mut lnets: Vec<Vec<usize>> = cell.nets.iter().map(|n| m.net_to_layout.get(&n.name).cloned().unwrap_or_default()).collect();
    {
        let t = TOL_NM * 1e-9 / meters_per_db;
        let nets_at = |p: [f64; 2], layers: &[String]| -> HashSet<usize> {
            let (x, y) = (p[0] * per_um, p[1] * per_um);
            let mut o = HashSet::new();
            for l in layers {
                if let Some(s) = shapes.get(l.as_str()) {
                    for (id, rs) in s.iter() {
                        if rs.iter().any(|r| x >= r[0] as f64 - t && x <= r[2] as f64 + t && y >= r[1] as f64 - t && y <= r[3] as f64 + t) {
                            o.insert(*id);
                        }
                    }
                }
            }
            o
        };
        let gl: Vec<String> = gate_layer.iter().cloned().collect();
        let sl: Vec<String> = sd_layer.iter().cloned().collect();
        let mut owner: HashMap<usize, String> = HashMap::new();
        for (k, net) in cell.nets.iter().enumerate() {
            let pts: Vec<(usize, [f64; 2])> = net.nodes.iter().filter_map(|&i| cell.nodes[i].xy.map(|p| (i, p))).collect();
            if pts.is_empty() {
                continue;
            }
            let mut count: BTreeMap<usize, usize> = BTreeMap::new();
            let mut on_lvs = 0;
            for (i, p) in &pts {
                // a device terminal point counts only its terminal layer: it often sits on a gate edge
                // or under another net's wire
                let t = &cell.nodes[*i].term;
                let layers = if well_net[k] {
                    &bulk_layers
                } else if t.iter().any(|x| x.1 == 'g') && !gl.is_empty() {
                    &gl
                } else if t.iter().any(|x| x.1 == 'd' || x.1 == 's') && !sl.is_empty() {
                    &sl
                } else {
                    &node_layers
                };
                let s = nets_at(*p, layers);
                if lnets[k].iter().any(|id| s.contains(id)) {
                    on_lvs += 1;
                }
                for id in s {
                    *count.entry(id).or_insert(0) += 1;
                }
            }
            let best = count.values().copied().max().unwrap_or(0);
            let tops: Vec<usize> = count.iter().filter(|(_, v)| **v == best).map(|(id, _)| *id).collect();
            if best > on_lvs && tops.len() == 1 {
                notes.push(format!("net {}: LVS maps it to layout net(s) {:?}, but {} of its {} node points lie on layout net {} ({} on the LVS net); using layout net {}", net.name, lnets[k], best, pts.len(), tops[0], on_lvs, tops[0]));
                lnets[k] = vec![tops[0]];
            }
            for &id in &lnets[k] {
                if let Some(o) = owner.insert(id, net.name.clone()) {
                    notes.push(format!("layout net {} is used for both {} and {}", id, o, net.name));
                }
            }
        }
    }
    let at = |p: [f64; 2], ids: &[usize], tol_nm: f64, layers: &[String]| -> Vec<String> {
        let (x, y) = (p[0] * per_um, p[1] * per_um);
        let t = tol_nm * 1e-9 / meters_per_db;
        let mut out = vec![];
        for l in layers {
            let s = match shapes.get(l.as_str()) {
                Some(s) => s,
                None => continue,
            };
            let hit = ids.iter().any(|id| s.get(id).map_or(false, |rs| rs.iter().any(|r| x >= r[0] as f64 - t && x <= r[2] as f64 + t && y >= r[1] as f64 - t && y <= r[3] as f64 + t)));
            if hit {
                out.push(l.clone());
            }
        }
        out
    };
    // pin labels: text -> (point µm, conductor the rules attach the label to)
    let mut pins: HashMap<String, Vec<([f64; 2], String)>> = HashMap::new();
    for (src, to, _) in &rules.labels {
        if !node_layers.contains(to) && !bulk_layers.contains(to) {
            continue;
        }
        if let Some(k) = label_layer_key(src, ex) {
            for t in &flat.texts {
                if (t.layer as i32, t.dt as i32) == k {
                    pins.entry(t.string.clone()).or_default().push(([t.x as f64 / per_um, t.y as f64 / per_um], to.clone()));
                }
            }
        }
    }
    let n = cell.nodes.len();
    let mut out: Vec<NodeLayer> = vec![NodeLayer::default(); n];
    let mut from: Vec<String> = vec![String::new(); n];
    for c in &cell.caps {
        if c.xy.is_some() && from[c.a].is_empty() && cell.nodes[c.a].xy == c.xy {
            from[c.a] = c.name.clone();
        }
        if let Some(b) = c.b {
            if c.xy2.is_some() && from[b].is_empty() && cell.nodes[b].xy == c.xy2 {
                from[b] = c.name.clone();
            }
        }
    }
    let mut has_rc = vec![false; n];
    for r in &cell.res {
        has_rc[r.a] = true;
        has_rc[r.b] = true;
    }
    for c in &cell.caps {
        has_rc[c.a] = true;
        if let Some(b) = c.b {
            has_rc[b] = true;
        }
    }
    let term_layer = |i: usize| -> Option<String> {
        let t = &cell.nodes[i].term;
        if t.iter().any(|x| x.1 == 'g') {
            gate_layer.clone()
        } else if t.iter().any(|x| x.1 == 'd' || x.1 == 's') {
            sd_layer.clone()
        } else {
            None
        }
    };
    let is_port = |name: &str| cell.ports.iter().any(|p| p == name) || cell.subckt.ports.iter().any(|p| p == name);
    let mut ambiguous = vec![];
    let mut internal = vec![];
    for i in 0..n {
        let nd = &cell.nodes[i];
        let o = &mut out[i];
        let net = &cell.nets[nd.net].name;
        o.xy = nd.xy;
        o.xy_from = if nd.xy.is_some() { if from[i].is_empty() { "R line".into() } else { from[i].clone() } } else { String::new() };
        let port_pins: Vec<([f64; 2], String)> = if is_port(&nd.name) { pins.get(&nd.name).cloned().unwrap_or_default() } else { vec![] };
        if o.xy.is_none() && !port_pins.is_empty() {
            // the label on a shape of this net
            let pick = port_pins.iter().find(|(p, _)| !at(*p, &lnets[nd.net], TOL_NM, layers_of(nd.net)).is_empty()).or(port_pins.first());
            if let Some((p, _)) = pick {
                o.xy = Some(*p);
                o.xy_from = "pin label".into();
            }
        }
        let tl = term_layer(i);
        if !has_rc[i] && tl.is_none() {
            o.how = if nd.term.is_empty() { "no R, C or device on this node".into() } else { "MOSFET bulk only, no R or C".into() };
            continue;
        }
        if lnets[nd.net].is_empty() {
            match tl {
                Some(l) => {
                    o.layer = Some(l);
                    o.src = Src::Terminal;
                    o.how = format!("device terminal (net {} has no layout net)", net);
                }
                None => o.how = format!("net {} has no layout net (LVS did not map it)", net),
            }
            continue;
        }
        let p = match o.xy {
            Some(p) => p,
            None => {
                match tl {
                    Some(l) => {
                        o.layer = Some(l);
                        o.src = Src::Terminal;
                        o.how = "device terminal, no coordinate".into();
                    }
                    None => internal.push(i),
                }
                continue;
            }
        };
        let mut c = at(p, &lnets[nd.net], TOL_NM, layers_of(nd.net));
        let mut near = false;
        if c.is_empty() {
            c = at(p, &lnets[nd.net], NEAR_NM, layers_of(nd.net));
            near = !c.is_empty();
        }
        o.cuts = at(p, &lnets[nd.net], if near { NEAR_NM } else { TOL_NM }, &cut_layers);
        o.candidates = c.clone();
        let nearly = if near { format!(" (within {} nm)", NEAR_NM) } else { String::new() };
        let pin = port_pins.iter().find(|(q, l)| (q[0] - p[0]).abs() <= tol_um && (q[1] - p[1]).abs() <= tol_um && c.contains(l)).map(|x| x.1.clone());
        if let Some(l) = pin {
            o.how = format!("port at its pin label on {} ({} at the point)", l, c.join("+"));
            o.layer = Some(l);
            o.src = Src::Pin;
        } else if c.len() == 1 {
            o.layer = Some(c[0].clone());
            o.src = Src::Shape;
            o.how = format!("only {} of net {} at the point{}", c[0], net, nearly);
        } else if c.is_empty() {
            match tl {
                Some(l) => {
                    o.layer = Some(l);
                    o.src = Src::Terminal;
                    o.how = format!("device terminal; no shape of net {} within {} nm", net, NEAR_NM);
                }
                None => o.how = format!("no shape of net {} within {} nm of ({}, {})", net, NEAR_NM, p[0], p[1]),
            }
        } else if let Some(l) = tl.filter(|l| c.contains(l)) {
            o.layer = Some(l.clone());
            o.src = Src::Terminal;
            o.how = format!("device terminal on {}{}", c.join("+"), nearly);
        } else {
            ambiguous.push(i);
        }
    }
    // R adjacency: (other node, resistor)
    let mut adj: Vec<Vec<(usize, usize)>> = vec![vec![]; n];
    for (k, r) in cell.res.iter().enumerate() {
        if r.a != r.b {
            adj[r.a].push((r.b, k));
            adj[r.b].push((r.a, k));
        }
    }
    // stacks: the candidate that the most strongly attributed R neighbours (pin, single shape, device
    // terminal) at other points are on. No chains through other stack decisions.
    let strong = |s: Src| matches!(s, Src::Tag | Src::Pin | Src::Shape | Src::Terminal);
    let mut set = vec![];
    for &i in &ambiguous {
        let mut score: BTreeMap<usize, usize> = BTreeMap::new();
        for &(j, _) in &adj[i] {
            let same_point = match (out[i].xy, out[j].xy) {
                (Some(p), Some(q)) => (p[0] - q[0]).abs() <= tol_um && (p[1] - q[1]).abs() <= tol_um,
                _ => false,
            };
            if same_point || !strong(out[j].src) {
                continue;
            }
            if let Some(l) = &out[j].layer {
                if let Some(k) = out[i].candidates.iter().position(|c| c == l) {
                    *score.entry(k).or_insert(0) += 1;
                }
            }
        }
        let best = score.values().copied().max().unwrap_or(0);
        let top: Vec<usize> = score.iter().filter(|(_, v)| **v == best).map(|(k, _)| *k).collect();
        if best > 0 && top.len() == 1 {
            set.push((i, Some((top[0], best))));
        } else {
            set.push((i, None));
        }
    }
    for (i, pick) in set {
        let o = &mut out[i];
        let cuts = if o.cuts.is_empty() { String::new() } else { format!(", {} at the point", o.cuts.join("+")) };
        match pick {
            Some((k, best)) => {
                let l = o.candidates[k].clone();
                o.how = format!("{} stack{}; {} attributed R neighbour(s) on {}", o.candidates.join("+"), cuts, best, l);
                o.layer = Some(l);
                o.src = Src::Neighbors;
            }
            None => {
                let l = o.candidates.last().unwrap().clone();
                o.how = format!("{} stack{}; no attributed R neighbour decides, the top layer {} is picked", o.candidates.join("+"), cuts, l);
                o.layer = Some(l);
                o.src = Src::Stack;
            }
        }
    }
    // layers each node is known to be on. Stack picks and internal nodes are passed through
    let sets: Vec<Option<Vec<String>>> = out.iter().map(|o| if o.src == Src::Stack { None } else { o.layer.clone().map(|l| vec![l]) }).collect();
    let order = |l: &String| (st.h(l), l.clone());
    // terminal shapes of a device terminal node: (layer, layout net, rect) of the gate or source/drain
    // layer holding its point, or the device's point ($X $Y at the channel's left edge) without one
    let term_rects = |i: usize| -> Vec<(String, usize, usize)> {
        let nd = &cell.nodes[i];
        let t = TOL_NM * 1e-9 / meters_per_db;
        let mut v = vec![];
        for &(di, kind) in &nd.term {
            let lay = match kind {
                'g' => gate_layer.clone(),
                'd' | 's' => sd_layer.clone(),
                _ => None,
            };
            let lay = match lay {
                Some(l) => l,
                None => continue,
            };
            let len = cell.subckt.mos.get(di).map(|d| d.l * 1e6).filter(|v| v.is_finite()).unwrap_or(0.0);
            let pts: Vec<[f64; 2]> = match (nd.xy, cell.devices.get(di).and_then(|d| d.xy)) {
                (Some(p), _) => vec![p],
                (None, Some(q)) if kind == 'g' => vec![[q[0] + len / 2.0, q[1]]],
                (None, Some(q)) => vec![q, [q[0] + len, q[1]]],
                _ => vec![],
            };
            if let Some(s) = shapes.get(lay.as_str()) {
                for &id in &lnets[nd.net] {
                    for (ri, r) in s.get(&id).map(|x| x.as_slice()).unwrap_or(&[]).iter().enumerate() {
                        if pts.iter().any(|p| {
                            let (x, y) = (p[0] * per_um, p[1] * per_um);
                            x >= r[0] as f64 - t && x <= r[2] as f64 + t && y >= r[1] as f64 - t && y <= r[3] as f64 + t
                        }) {
                            v.push((lay.clone(), id, ri));
                        }
                    }
                }
            }
        }
        v
    };
    // a node without a point whose R neighbours include two device terminals on one gate (or one
    // source/drain) shape is the junction on that shape: poly branching to fingers, shared diffusion
    let junction = |u: usize| -> Option<(String, usize)> {
        let mut by: HashMap<(String, usize, usize), HashSet<usize>> = HashMap::new();
        for &(x, _) in &adj[u] {
            if cell.nodes[x].term.is_empty() {
                continue;
            }
            for key in term_rects(x) {
                by.entry(key).or_default().insert(x);
            }
        }
        let mut hits: Vec<(String, usize)> = by.into_iter().filter(|(_, v)| v.len() >= 2).map(|(k, v)| (k.0, v.len())).collect();
        hits.sort();
        hits.dedup_by(|a, b| a.0 == b.0);
        if hits.len() == 1 { hits.pop() } else { None }
    };
    // neighbours on several layers do not decide: one reduced R can span several layers (diff to M1 on the ASAP7 rails)
    let undecided = |e: &str| if e.starts_with("joins") || e.starts_with("could be") { " (one R of a reduced network can span several layers, so its neighbours do not decide)" } else { "" };
    // internal nodes without a point
    for &i in &internal {
        let found = boundary(i, None, &adj, &sets);
        let j = junction(i);
        let o = &mut out[i];
        if let Some((l, k)) = j {
            o.how = format!("no coordinate; joins {} device terminals on one {} shape", k, l);
            o.layer = Some(l);
            o.src = Src::Inferred;
            continue;
        }
        match common_layer(&found, &order) {
            Ok(l) => {
                o.how = format!("no coordinate; {} on the {} nearest attributed node(s) over resistors", l, found.len());
                o.layer = Some(l);
                o.src = Src::Inferred;
            }
            Err(e) => o.how = format!("no coordinate; over resistors it {}{}", e, undecided(&e)),
        }
    }
    let nm = |i: usize| cell.nodes[i].name.as_str();
    let mut res = vec![];
    // R decided as the vertical connection at one point of a stack
    let mut vert = vec![false; cell.res.len()];
    for (k, r) in cell.res.iter().enumerate() {
        let mut o = ResLayer::default();
        let mut why = vec![];
        for (e, x) in [r.a, r.b].into_iter().enumerate() {
            if out[x].layer.is_some() && out[x].src != Src::Inferred {
                o.ends[e] = out[x].layer.clone();
                o.ends_src[e] = out[x].src;
                continue;
            }
            if out[x].xy.is_some() {
                why.push(format!("{}: {}", nm(x), out[x].how));
                continue;
            }
            if let Some((l, n)) = junction(x) {
                why.push(format!("{} joins {} device terminals on one {} shape", nm(x), n, l));
                o.ends[e] = Some(l);
                o.ends_src[e] = Src::Inferred;
                continue;
            }
            match common_layer(&boundary(x, Some(k), &adj, &sets), &order) {
                Ok(l) => {
                    why.push(format!("{} inferred {} over the other resistors", nm(x), l));
                    o.ends[e] = Some(l);
                    o.ends_src[e] = Src::Inferred;
                }
                Err(er) => why.push(format!("{} has no coordinate and over the other resistors {}{}", nm(x), er, undecided(&er))),
            }
        }
        let same_point = match (out[r.a].xy, out[r.b].xy) {
            (Some(p), Some(q)) => (p[0] - q[0]).abs() <= tol_um && (p[1] - q[1]).abs() <= tol_um,
            _ => false,
        };
        let mut stack: Vec<String> = out[r.a].candidates.iter().chain(out[r.b].candidates.iter()).cloned().collect();
        stack.sort_by_key(|l| order(l));
        stack.dedup();
        if let Some(t) = &r.tag {
            o.layer = Some(t.clone());
            o.how = "tag".into();
        } else {
            let extra = if why.is_empty() { String::new() } else { format!("; {}", why.join("; ")) };
            let mut cuts: Vec<String> = out[r.a].cuts.iter().chain(out[r.b].cuts.iter()).cloned().collect();
            cuts.sort_by_key(|l| order(l));
            cuts.dedup();
            let decided_apart = matches!((&o.ends[0], &o.ends[1]), (Some(a), Some(b)) if a != b);
            match (&o.ends[0], &o.ends[1]) {
                _ if same_point && stack.len() > 1 && !decided_apart => {
                    // two nodes at one point of a stack: a vertical connection, not a wire on one layer
                    o.layer = Some(if cuts.is_empty() { st.join(&stack[0], stack.last().unwrap()) } else { format!("{} ({})", cuts.join("+"), stack.join("+")) });
                    o.how = format!("both ends at one point of the {} stack: the vertical connection there", stack.join("+"));
                    vert[k] = true;
                }
                (Some(a), Some(b)) if a == b => {
                    o.layer = Some(a.clone());
                    o.how = format!("both ends on {}{}", a, extra);
                }
                (Some(a), Some(b)) => {
                    o.layer = Some(st.join(a, b));
                    o.how = format!("{} on {}, {} on {}{}", nm(r.a), a, nm(r.b), b, extra);
                }
                _ => o.how = format!("end unknown: {}", why.join("; ")),
            }
        }
        res.push(o);
    }
    // value match, discrete equality only: an unknown R whose value equals (within 1e-5) a value that at least two
    // R placed by position share in one class (both ends by pin, shape, terminal or tag, or both at one point of a
    // stack), and no attributed R of another class has, takes that class
    let firm_src = |s: Src| matches!(s, Src::Tag | Src::Pin | Src::Shape | Src::Terminal);
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-5 * a.abs().max(b.abs());
    let firm: Vec<(f64, String)> = cell.res.iter().zip(&res).enumerate().filter(|(k, (r, o))| o.layer.is_some() && r.ohms.is_finite() && r.ohms > 0.0 && (r.tag.is_some() || vert[*k] || o.ends_src.iter().all(|s| firm_src(*s)))).map(|(_, (r, o))| (r.ohms, o.layer.clone().unwrap_or_default())).collect();
    let known: Vec<(f64, String)> = cell.res.iter().zip(&res).filter(|(_, o)| o.layer.is_some()).map(|(r, o)| (r.ohms, o.layer.clone().unwrap_or_default())).collect();
    let mut classes: Vec<(f64, String, usize, usize)> = vec![];
    for (v, c) in &firm {
        if classes.iter().any(|x| close(x.0, *v)) {
            continue;
        }
        let n = firm.iter().filter(|(w, d)| d == c && close(*w, *v)).count();
        if n >= 2 && !known.iter().any(|(w, d)| d != c && close(*w, *v)) {
            classes.push((*v, c.clone(), n, 0));
        }
    }
    for (k, r) in cell.res.iter().enumerate() {
        if res[k].layer.is_some() || r.tag.is_some() || !r.ohms.is_finite() {
            continue;
        }
        let Some(ci) = classes.iter().position(|x| close(x.0, r.ohms)) else { continue };
        let (n, c) = (classes[ci].2, classes[ci].1.clone());
        // an end placed by position on a layer the class does not name rules the match out
        let names: HashSet<&str> = c.split(|ch: char| !ch.is_alphanumeric() && ch != '_').filter(|t| !t.is_empty()).collect();
        let clash = [r.a, r.b].iter().filter(|&&x| firm_src(out[x].src)).find_map(|&x| out[x].layer.clone().filter(|l| !names.contains(l.as_str())));
        let o = &mut res[k];
        match clash {
            Some(l) => o.how = format!("{}. Its value {} ohm equals {} R placed as {}, but an end is on {}, so it is not used", o.how, r.ohms, n, c, l),
            None => {
                o.how = format!("value {} ohm equals {} R placed by position as {} in this cell (not placed by its own ends: {})", r.ohms, n, c, o.how);
                o.layer = Some(c);
                o.by_value = true;
                classes[ci].3 += 1;
            }
        }
    }
    for (v, c, n, m) in &classes {
        notes.push(format!("value class {} ohm = {} ({} R placed by position), {} unknown R take it", v, c, n, m));
    }
    let side = |node: usize, xy: Option<[f64; 2]>, tag: &Option<String>| -> (Option<String>, String) {
        if let Some(t) = tag {
            return (Some(t.clone()), "tag".into());
        }
        let o = &out[node];
        if let (Some(p), Some(q)) = (xy, o.xy) {
            if (p[0] - q[0]).abs() > 1e-6 || (p[1] - q[1]).abs() > 1e-6 {
                let c = at(p, &lnets[cell.nodes[node].net], TOL_NM, layers_of(cell.nodes[node].net));
                if let Some(l) = o.layer.as_ref().filter(|l| c.contains(l)) {
                    return (Some(l.clone()), format!("node {} layer, also at the element's own point", nm(node)));
                }
                if c.len() == 1 {
                    return (Some(c[0].clone()), format!("only {} at the element's own point", c[0]));
                }
            }
        }
        (o.layer.clone(), if o.layer.is_some() { format!("node {}", nm(node)) } else { format!("node {}: {}", nm(node), o.how) })
    };
    let mut caps = vec![];
    for c in &cell.caps {
        let (a, ha) = side(c.a, c.xy, &c.tag_a);
        let (b, hb) = match c.b {
            Some(b) => {
                let (l, h) = side(b, c.xy2, &c.tag_b);
                (l, Some(h))
            }
            None => (None, None),
        };
        let how = match hb {
            Some(hb) => format!("{} | {}", ha, hb),
            None => ha,
        };
        caps.push(CapLayer { a, b, how });
    }
    Attribution { nodes: out, res, caps, node_layers, cut_layers, net_layout: lnets, notes, sd_layer: sd_layer.clone() }
}

/// Compare the LPE devices with a CDL subckt up to net naming: same device names (fingers
/// `MM3@2` belong to `MM3`), types, models (ignoring case), lengths, summed fins and widths, and one
/// net mapping (ports by name, ignoring case) under which every device connects the same way
/// (source and drain may swap). Returns the differences, empty means equal.
pub fn compare_devices(lpe: &Subckt, cdl: &Subckt) -> Vec<String> {
    let mut diffs = vec![];
    let base = |n: &str| -> String { n.split('@').next().unwrap_or(n).to_string() };
    struct G {
        typ: String,
        model: String,
        l: f64,
        fins: f64,
        w: f64,
        g: String,
        ds: [String; 2],
        b: String,
    }
    let fins = |d: &Mos| (if d.nfin.is_finite() { d.nfin } else { 0.0 }) * (if d.m.is_finite() && d.m != 0.0 { d.m } else { 1.0 }) * (if d.nf.is_finite() && d.nf != 0.0 { d.nf } else { 1.0 });
    let width = |d: &Mos| (if d.w.is_finite() { d.w } else { 0.0 }) * (if d.m.is_finite() && d.m != 0.0 { d.m } else { 1.0 });
    let mut groups: BTreeMap<String, G> = BTreeMap::new();
    for d in &lpe.mos {
        let k = base(&d.name).to_uppercase();
        match groups.get_mut(&k) {
            None => {
                groups.insert(k, G { typ: d.typ.clone(), model: d.model.to_lowercase(), l: d.l, fins: fins(d), w: width(d), g: d.g.clone(), ds: [d.d.clone(), d.s.clone()], b: d.b.clone() });
            }
            Some(g) => {
                let same_ds = (g.ds[0] == d.d && g.ds[1] == d.s) || (g.ds[0] == d.s && g.ds[1] == d.d);
                if g.typ != d.typ || g.model != d.model.to_lowercase() || g.g != d.g || g.b != d.b || !same_ds || (g.l - d.l).abs() > 1e-12 {
                    diffs.push(format!("finger {} differs from the other fingers of {}", d.name, base(&d.name)));
                }
                g.fins += fins(d);
                g.w += width(d);
            }
        }
    }
    let mut cdev: BTreeMap<String, &Mos> = BTreeMap::new();
    for d in &cdl.mos {
        if cdev.insert(d.name.to_uppercase(), d).is_some() {
            diffs.push(format!("CDL device {} appears twice", d.name));
        }
    }
    for k in groups.keys() {
        if !cdev.contains_key(k) {
            diffs.push(format!("LPE device {} is not in the CDL", k));
        }
    }
    for k in cdev.keys() {
        if !groups.contains_key(k) {
            diffs.push(format!("CDL device {} is not in the LPE netlist", k));
        }
    }
    let close = |a: f64, b: f64| (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-30) || (a.is_nan() && b.is_nan());
    let mut pairs: Vec<(&G, &Mos)> = vec![];
    for (k, g) in &groups {
        let c = match cdev.get(k) {
            Some(c) => *c,
            None => continue,
        };
        if g.typ != c.typ || g.model != c.model.to_lowercase() {
            diffs.push(format!("{}: {} {} in the LPE netlist, {} {} in the CDL", k, g.typ, g.model, c.typ, c.model));
        }
        if !close(g.l, c.l) {
            diffs.push(format!("{}: L {} vs {}", k, g.l, c.l));
        }
        if !close(g.fins, fins(c)) {
            diffs.push(format!("{}: fins {} vs {}", k, g.fins, fins(c)));
        }
        if !close(g.w, width(c) * if c.nf.is_finite() && c.nf != 0.0 { c.nf } else { 1.0 }) && !close(g.w, width(c)) {
            diffs.push(format!("{}: W total {} vs {}", k, g.w, width(c)));
        }
        pairs.push((g, c));
    }
    if !diffs.is_empty() {
        return diffs;
    }
    // one-to-one net mapping, ports fixed by name
    let mut fw: HashMap<String, String> = HashMap::new();
    let mut bw: HashMap<String, String> = HashMap::new();
    for p in &lpe.ports {
        if let Some(q) = cdl.ports.iter().find(|q| q.eq_ignore_ascii_case(p)) {
            fw.insert(p.clone(), q.clone());
            bw.insert(q.clone(), p.clone());
        } else {
            diffs.push(format!("LPE port {} is not a CDL port", p));
        }
    }
    for q in &cdl.ports {
        if !lpe.ports.iter().any(|p| p.eq_ignore_ascii_case(q)) {
            diffs.push(format!("CDL port {} is not an LPE port", q));
        }
    }
    if !diffs.is_empty() {
        return diffs;
    }
    fn bind(a: &str, c: &str, fw: &mut HashMap<String, String>, bw: &mut HashMap<String, String>, undo: &mut Vec<String>) -> bool {
        if let Some(x) = fw.get(a) {
            return x == c;
        }
        if bw.contains_key(c) {
            return false;
        }
        fw.insert(a.to_string(), c.to_string());
        bw.insert(c.to_string(), a.to_string());
        undo.push(a.to_string());
        true
    }
    fn go(i: usize, pairs: &[(&G, &Mos)], fw: &mut HashMap<String, String>, bw: &mut HashMap<String, String>, steps: &mut usize) -> bool {
        *steps += 1;
        if i == pairs.len() {
            return true;
        }
        if *steps > 200_000 {
            return false;
        }
        let (g, c) = pairs[i];
        for swap in [false, true] {
            let (d, s) = if swap { (&g.ds[1], &g.ds[0]) } else { (&g.ds[0], &g.ds[1]) };
            let mut undo = vec![];
            let ok = bind(&g.g, &c.g, fw, bw, &mut undo) && bind(&g.b, &c.b, fw, bw, &mut undo) && bind(d, &c.d, fw, bw, &mut undo) && bind(s, &c.s, fw, bw, &mut undo);
            if ok && go(i + 1, pairs, fw, bw, steps) {
                return true;
            }
            for a in undo {
                if let Some(cn) = fw.remove(&a) {
                    bw.remove(&cn);
                }
            }
        }
        false
    }
    let mut steps = 0;
    if !go(0, &pairs, &mut fw, &mut bw, &mut steps) {
        diffs.push(format!("no net mapping connects every device the same way{}", if steps > 200_000 { " (search limit reached)" } else { "" }));
        // the devices whose terminals differ by net name (ignoring case)
        for (g, c) in &pairs {
            let u = |s: &str| s.to_uppercase();
            let same_ds = (u(&g.ds[0]) == u(&c.d) && u(&g.ds[1]) == u(&c.s)) || (u(&g.ds[0]) == u(&c.s) && u(&g.ds[1]) == u(&c.d));
            if u(&g.g) != u(&c.g) || !same_ds {
                diffs.push(format!("{}: LPE gate {} s/d {} {}, CDL gate {} s/d {} {}", c.name, g.g, g.ds[0], g.ds[1], c.g, c.d, c.s));
            }
        }
    }
    diffs
}

/// A subckt as a layout extraction (one net per subckt net, port names as labels, one finger per
/// device with nfin * m * nf fins), so `devmatch::match_cell` can compare two netlists LVS-style.
pub fn subckt_as_extraction(sc: &Subckt) -> Extraction {
    let mut ids: HashMap<String, usize> = HashMap::new();
    let mut ex = Extraction::default();
    let mut id = |n: &str, ex: &mut Extraction| -> usize {
        if let Some(&i) = ids.get(n) {
            return i;
        }
        let i = ex.nets.len();
        let names = if sc.ports.iter().any(|p| p == n) { vec![n.to_string()] } else { vec![] };
        ex.nets.push(crate::extract::ExNet { id: i, name: names.first().cloned(), names, layers: vec![] });
        ids.insert(n.to_string(), i);
        i
    };
    for p in &sc.ports {
        id(p, &mut ex);
    }
    for d in &sc.mos {
        let (g, a, b, bulk) = (id(&d.g, &mut ex), id(&d.d, &mut ex), id(&d.s, &mut ex), id(&d.b, &mut ex));
        let f = |v: f64| if v.is_finite() && v != 0.0 { v } else { 1.0 };
        let nfin = (if d.nfin.is_finite() { d.nfin } else { 0.0 }) * f(d.m) * f(d.nf);
        ex.devices.push(crate::extract::ExDevice { typ: d.typ.clone(), gate: g as i64, sd: vec![a, b], bulk: bulk as i64, nfin, ..Default::default() });
    }
    ex
}

/// LPE subckt against a CDL subckt: device by device (`compare_devices`) and LVS-style
/// (series and parallel order free, `devmatch::match_cell` with the CDL as the "layout").
pub struct CdlCompare {
    /// differences device by device. Empty: same devices, same connections up to net naming
    pub exact: Vec<String>,
    /// LVS-style match
    pub lvs_ok: bool,
    pub lvs: String,
}

pub fn compare_to_cdl(lpe: &Subckt, cdl: &Subckt) -> CdlCompare {
    let exact = compare_devices(lpe, cdl);
    let ex = subckt_as_extraction(cdl);
    let m = crate::devmatch::match_cell(&ex, lpe, true);
    let lvs = if m.ok {
        format!("LVS-equivalent ({} reduced groups)", m.matched_groups)
    } else {
        format!(
            "not LVS-equivalent (groups CDL {} LPE {} matched {}){}{}{}",
            m.layout_groups,
            m.cdl_groups,
            m.matched_groups,
            if m.conflicts.is_empty() { String::new() } else { format!(" | {}", m.conflicts.join("; ")) },
            if m.unmatched_layout.is_empty() { String::new() } else { format!(" | CDL only: {}", m.unmatched_layout.join("; ")) },
            if m.unmatched_cdl.is_empty() { String::new() } else { format!(" | LPE only: {}", m.unmatched_cdl.join("; ")) }
        )
    };
    CdlCompare { exact, lvs_ok: m.ok, lvs }
}

// ---------- metal segments ----------

/// what a metal segment of the post-layout netlist is
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SegKind {
    /// the wire of one R between its two nodes, on one conductor layer
    Wire,
    /// the via or contact of one R between two layers
    Via,
    /// conductor under a capacitor's point that no wire R covers (a source/drain region, a pin, a gate end)
    Patch,
    /// the routing of one R that turns or changes layer between its two nodes (an extractor merged several pieces)
    Path,
}

/// one physical piece of conductor as the post-layout netlist splits it into R and C
#[derive(Clone, Debug)]
pub struct Segment {
    pub kind: SegKind,
    /// the conductor layer (Wire, Patch) or the R's layer text, such as "V0 (LISD-M1)" (Via)
    pub layer: String,
    /// LPE net
    pub net: usize,
    /// conductor rectangles of the segment (GDS database units)
    pub rects: Vec<[i32; 4]>,
    /// end points in database units: the two node points of a Wire or Via, the capacitor point of a Patch twice
    pub ends: [[f64; 2]; 2],
    /// the R of a Wire or Via
    pub res: Option<usize>,
    /// the nodes at the two ends (a Patch: its node twice)
    pub nodes: [usize; 2],
}

/// one side of a capacitor: the segment its point lies on
#[derive(Clone, Debug)]
pub struct CapSide {
    pub seg: usize,
    /// the capacitor's point on this side (database units)
    pub at: [f64; 2],
}

/// where the position of a node comes from
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PosFrom {
    /// its own point in the netlist (or the pin label of a port)
    Own,
    /// the terminal of the layout device the LVS matched to its device
    Device,
    /// placed between its R neighbours by the R values
    Resistors,
    None,
}

/// the parasitics of a cell split into metal segments the way the post-layout netlist splits them
#[derive(Clone, Debug, Default)]
pub struct Segmentation {
    /// position of every LPE node (database units): its own point, else placed between its R neighbours
    pub pos: Vec<Option<[f64; 2]>>,
    /// nodes without a point of their own that got a position (from a device or from their R neighbours)
    pub inferred: Vec<bool>,
    /// where each node position comes from
    pub from: Vec<PosFrom>,
    pub segs: Vec<Segment>,
    /// the segment of every R, or why it has none
    pub res: Vec<Result<usize, String>>,
    /// the two sides of every capacitor (side B only for a coupling capacitor)
    pub caps: Vec<[Option<CapSide>; 2]>,
}

fn inside(r: &[i32; 4], p: [f64; 2], tol: f64) -> bool {
    p[0] >= r[0] as f64 - tol && p[0] <= r[2] as f64 + tol && p[1] >= r[1] as f64 - tol && p[1] <= r[3] as f64 + tol
}

/// the point of a rectangle nearest to `p`, and its squared distance
fn nearest_in(r: &[i32; 4], p: [f64; 2]) -> ([f64; 2], f64) {
    let q = [p[0].clamp(r[0] as f64, r[2] as f64), p[1].clamp(r[1] as f64, r[3] as f64)];
    (q, (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2))
}

/// the part of a wire's rectangles between two points on it: every rectangle that reaches the box of the two
/// points is cut along its long side to that box (widened by half the rectangle's width, so the wire keeps its
/// ends) and kept across its full width
pub fn corridor(rects: &[[i32; 4]], a: [f64; 2], b: [f64; 2]) -> Vec<[i32; 4]> {
    let mut out = vec![];
    for r in rects {
        let (w, h) = ((r[2] - r[0]) as f64, (r[3] - r[1]) as f64);
        let e = w.min(h) / 2.0;
        let bx = [a[0].min(b[0]) - e, a[1].min(b[1]) - e, a[0].max(b[0]) + e, a[1].max(b[1]) + e];
        if (r[2] as f64) < bx[0] || (r[0] as f64) > bx[2] || (r[3] as f64) < bx[1] || (r[1] as f64) > bx[3] {
            continue;
        }
        let mut c = [(r[0] as f64).max(bx[0]), (r[1] as f64).max(bx[1]), (r[2] as f64).min(bx[2]), (r[3] as f64).min(bx[3])];
        if w >= h {
            c[1] = r[1] as f64;
            c[3] = r[3] as f64;
        } else {
            c[0] = r[0] as f64;
            c[2] = r[2] as f64;
        }
        if c[0] < c[2] && c[1] < c[3] {
            out.push([c[0].round() as i32, c[1].round() as i32, c[2].round() as i32, c[3].round() as i32]);
        }
    }
    out
}

/// Split the parasitics of `cell` into metal segments the way the netlist splits them, not by net:
/// 1. node positions: a node's own point (its first `$X $Y`, a DSPF point, a pin label); else for a device terminal
///    the terminal of the layout device the LVS matched (channel centre for a gate, the source/drain point nearest
///    the channel); else placed between its R neighbours with weights 1/R (on a chain of R that gives each inner
///    node the share of the length its R take; a via, with a small R, keeps its two ends together), then moved
///    onto the conductor of its layer (of its net when the layer is unknown);
/// 2. every R on one conductor layer is a Wire segment: the conductor of its net on that layer between its two
///    node points; every R between layers is a Via segment: the cut shapes of its net at its points;
/// 3. every capacitor side goes on the Wire segment of its layer and net that holds its own point (one that ends
///    at its node first); a point that no wire covers gets a Patch segment: the shapes of that layer and net under
///    the point (the nearest one when none is under it).
pub fn segment_parasitics(cell: &LpeCell, at: &Attribution, ex: &Extraction, m: &MatchResult, meters_per_db: f64) -> Segmentation {
    let tol = 1e-9 / meters_per_db;
    let um = |p: [f64; 2]| [p[0] * 1e-6 / meters_per_db, p[1] * 1e-6 / meters_per_db];
    let net_rects = |layer: &str, net: usize| -> Vec<[i32; 4]> {
        let by_net = ex.shapes.iter().find(|(l, _)| l == layer).map(|(_, s)| s);
        at.net_layout.get(net).into_iter().flatten().flat_map(|ln| by_net.and_then(|s| s.get(ln)).cloned().unwrap_or_default()).collect()
    };
    let n = cell.nodes.len();
    // 1. positions: the node's own point
    let mut pos: Vec<Option<[f64; 2]>> = (0..n).map(|i| at.nodes.get(i).and_then(|x| x.xy).or(cell.nodes[i].xy).map(um)).collect();
    let own: Vec<bool> = pos.iter().map(|p| p.is_some()).collect();
    // a device terminal without a point: from the layout device the LVS matched to its device, the channel centre for
    // a gate, else the point of its source/drain piece on the node's net nearest to the channel (where a DSPF puts the pin)
    let overlap = |a: &[i32; 4], b: &[i32; 4]| a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3];
    for i in 0..n {
        if pos[i].is_some() {
            continue;
        }
        let Some(&(d, t)) = cell.nodes[i].term.iter().find(|x| x.1 != 'b') else { continue };
        let Some(f) = cell.devices.get(d).and_then(|dev| m.dev_to_layout.get(&dev.name)).and_then(|fs| fs.first()).and_then(|&f| ex.devices.get(f)) else { continue };
        let Some(ch) = f.rects.iter().copied().reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]) else { continue };
        let cc = [(ch[0] + ch[2]) as f64 / 2.0, (ch[1] + ch[3]) as f64 / 2.0];
        if t == 'g' {
            pos[i] = Some(cc);
            continue;
        }
        // the pieces on the node's net, judged on the source/drain layer only (a wire above crosses the other piece)
        let lnets: Vec<usize> = at.net_layout.get(cell.nodes[i].net).cloned().unwrap_or_default();
        let sd_shapes = ex.shapes.iter().filter(|(l, _)| at.sd_layer.as_deref().map_or(true, |s| s == l));
        let sd_shapes: Vec<&HashMap<usize, Vec<[i32; 4]>>> = sd_shapes.map(|(_, s)| s).collect();
        let mine: Vec<&[i32; 4]> = f.sd_rects.iter().filter(|r| lnets.iter().any(|ln| sd_shapes.iter().any(|s| s.get(ln).map_or(false, |q| q.iter().any(|q| overlap(q, r)))))).collect();
        let pick = if mine.is_empty() { f.sd_rects.iter().collect::<Vec<_>>() } else { mine };
        if let Some((p, _)) = pick.iter().map(|r| nearest_in(r, cc)).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal)) {
            pos[i] = Some(p);
        }
    }
    let known: Vec<bool> = pos.iter().map(|p| p.is_some()).collect();
    let mut adj: Vec<Vec<(usize, f64)>> = vec![vec![]; n];
    for r in &cell.res {
        let w = 1.0 / r.ohms.abs().max(1e-3);
        adj[r.a].push((r.b, w));
        adj[r.b].push((r.a, w));
    }
    let avg = |i: usize, pos: &[Option<[f64; 2]>]| -> Option<[f64; 2]> {
        let (mut s, mut ws) = ([0.0; 2], 0.0);
        for &(j, w) in &adj[i] {
            if let Some(p) = pos[j] {
                s[0] += w * p[0];
                s[1] += w * p[1];
                ws += w;
            }
        }
        (ws > 0.0).then(|| [s[0] / ws, s[1] / ws])
    };
    // first guess outward from the placed nodes, then relax the unplaced ones to the weighted mean of their neighbours
    loop {
        let mut changed = false;
        for i in 0..n {
            if pos[i].is_none() {
                if let Some(p) = avg(i, &pos) {
                    pos[i] = Some(p);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    for _ in 0..5000 {
        let mut moved: f64 = 0.0;
        for i in 0..n {
            if known[i] || pos[i].is_none() {
                continue;
            }
            if let (Some(old), Some(p)) = (pos[i], avg(i, &pos)) {
                moved = moved.max((p[0] - old[0]).abs().max((p[1] - old[1]).abs()));
                pos[i] = Some(p);
            }
        }
        if moved < 0.01 {
            break;
        }
    }
    // a placed node lies on the conductor of its layer (on the nearest conductor of its net when its layer is unknown)
    for i in 0..n {
        if known[i] {
            continue;
        }
        let Some(p) = pos[i] else { continue };
        let q: Vec<[i32; 4]> = match at.nodes.get(i).and_then(|x| x.layer.as_deref()) {
            Some(layer) => net_rects(layer, cell.nodes[i].net),
            None => at.node_layers.iter().flat_map(|l| net_rects(l, cell.nodes[i].net)).collect(),
        };
        if !q.is_empty() && !q.iter().any(|r| inside(r, p, tol)) {
            let best = q.iter().map(|r| nearest_in(r, p)).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((np, _)) = best {
                pos[i] = Some(np);
            }
        }
    }
    let inferred: Vec<bool> = (0..n).map(|i| !own[i] && pos[i].is_some()).collect();
    let from: Vec<PosFrom> = (0..n)
        .map(|i| match (own[i], known[i], pos[i].is_some()) {
            (true, _, _) => PosFrom::Own,
            (false, true, _) => PosFrom::Device,
            (false, false, true) => PosFrom::Resistors,
            _ => PosFrom::None,
        })
        .collect();
    // 2. one segment per R
    let node_layers: HashSet<&str> = at.node_layers.iter().map(|s| s.as_str()).collect();
    let mut segs: Vec<Segment> = vec![];
    let mut res: Vec<Result<usize, String>> = vec![];
    for (k, r) in cell.res.iter().enumerate() {
        let (Some(pa), Some(pb)) = (pos[r.a], pos[r.b]) else {
            res.push(Err("an end node has no position (no point and no placed R neighbour)".into()));
            continue;
        };
        // the conductor layer(s) of a wire R, or None for an R between layers
        let same_point = (pa[0] - pb[0]).abs() <= tol && (pa[1] - pb[1]).abs() <= tol;
        let (wire_layers, layer): (Option<Vec<String>>, String) = match at.res.get(k).and_then(|x| x.layer.clone()) {
            // a well resistor (Magic: VPB through the n-well) runs in the well like a wire
            Some(l) if node_layers.contains(l.as_str()) || is_bulk_layer(&l) => (Some(vec![l.clone()]), l),
            Some(l) => (None, l),
            None => {
                // layer unknown from the netlist (a rail where conductors are stacked): the conductor layers of the
                // net that hold both end points and the wire between them, top first; several stay named as such
                let mut cands: Vec<String> = at
                    .node_layers
                    .iter()
                    .filter(|l| {
                        let q = net_rects(l, r.net);
                        q.iter().any(|x| inside(x, pa, tol)) && q.iter().any(|x| inside(x, pb, tol)) && !corridor(&q, pa, pb).is_empty()
                    })
                    .cloned()
                    .collect();
                cands.reverse();
                // no one conductor joins the two ends (a reduced R from a rail down to a source/drain region): the
                // conductors of the layers at its two ends, cut to the stretch between them, as a connection between layers
                let at_a: Vec<String> = at.node_layers.iter().filter(|l| net_rects(l, r.net).iter().any(|x| inside(x, pa, tol))).cloned().collect();
                let at_b: Vec<String> = at.node_layers.iter().filter(|l| net_rects(l, r.net).iter().any(|x| inside(x, pb, tol))).cloned().collect();
                if cands.is_empty() && !at_a.is_empty() && !at_b.is_empty() && !same_point {
                    let mut q = vec![];
                    for l in at_a.iter().chain(at_b.iter().filter(|l| !at_a.contains(l))) {
                        q.extend(net_rects(l, r.net));
                    }
                    let rects = corridor(&q, pa, pb);
                    if !rects.is_empty() {
                        res.push(Ok(segs.len()));
                        segs.push(Segment { kind: SegKind::Via, layer: format!("{}-{}", at_a.join("+"), at_b.join("+")), net: r.net, rects, ends: [pa, pb], res: Some(k), nodes: [r.a, r.b] });
                        continue;
                    }
                }
                if cands.is_empty() {
                    let on = |p: [f64; 2]| at.node_layers.iter().filter(|l| net_rects(l, r.net).iter().any(|x| inside(x, p, tol))).cloned().collect::<Vec<_>>().join("+");
                    let u = |p: [f64; 2]| format!("({:.4}, {:.4})", p[0] * meters_per_db * 1e6, p[1] * meters_per_db * 1e6);
                    res.push(Err(format!(
                        "layer unknown, and no conductor of its net holds both ends: {} at {} on [{}]{}, {} at {} on [{}]{}",
                        cell.nodes[r.a].name,
                        u(pa),
                        on(pa),
                        if known[r.a] { "" } else { " (placed)" },
                        cell.nodes[r.b].name,
                        u(pb),
                        on(pb),
                        if known[r.b] { "" } else { " (placed)" }
                    )));
                    continue;
                }
                let name = cands.join("|");
                if same_point {
                    (None, format!("stack {}", name))
                } else {
                    (Some(cands), name)
                }
            }
        };
        if let Some(wl) = wire_layers {
            let mut q = vec![];
            for l in &wl {
                q.extend(net_rects(l, r.net));
            }
            let mut rects = corridor(&q, pa, pb);
            if rects.is_empty() {
                rects = q.iter().filter(|x| inside(x, pa, tol) || inside(x, pb, tol)).cloned().collect();
            }
            if rects.is_empty() {
                res.push(Err(format!("no {} shape of its net at its end points", layer)));
                continue;
            }
            res.push(Ok(segs.len()));
            segs.push(Segment { kind: SegKind::Wire, layer, net: r.net, rects, ends: [pa, pb], res: Some(k), nodes: [r.a, r.b] });
        } else {
            let mid = [(pa[0] + pb[0]) / 2.0, (pa[1] + pb[1]) / 2.0];
            let mut rects: Vec<[i32; 4]> = at.cut_layers.iter().flat_map(|c| net_rects(c, r.net)).filter(|x| inside(x, pa, tol) || inside(x, pb, tol) || inside(x, mid, tol)).collect();
            if rects.is_empty() {
                // no cut shape at the point (a contact chain drawn on other layers): a 18 nm square marks it
                let h = 9e-9 / meters_per_db;
                rects.push([(mid[0] - h).round() as i32, (mid[1] - h).round() as i32, (mid[0] + h).round() as i32, (mid[1] + h).round() as i32]);
            }
            res.push(Ok(segs.len()));
            segs.push(Segment { kind: SegKind::Via, layer, net: r.net, rects, ends: [pa, pb], res: Some(k), nodes: [r.a, r.b] });
        }
    }
    // 3. capacitor sides on the segment under their point
    let mut patch: HashMap<(usize, String), Vec<usize>> = HashMap::new();
    let mut caps: Vec<[Option<CapSide>; 2]> = vec![];
    for (k, c) in cell.caps.iter().enumerate() {
        let cl = at.caps.get(k);
        let sides = [(Some(c.a), Some(c.net_a), cl.and_then(|x| x.a.clone()), c.xy), (c.b.filter(|_| c.coupling), c.net_b.filter(|_| c.coupling), cl.and_then(|x| x.b.clone()), c.xy2)];
        let mut out: [Option<CapSide>; 2] = [None, None];
        for (s, (node, net, layer, xy)) in sides.into_iter().enumerate() {
            let (Some(node), Some(net), Some(layer)) = (node, net, layer) else { continue };
            let Some(p) = xy.map(um).or(pos[node]) else { continue };
            let on = |i: &usize| segs[*i].kind == SegKind::Wire && segs[*i].net == net && segs[*i].layer == layer && segs[*i].rects.iter().any(|r| inside(r, p, tol));
            let wires: Vec<usize> = (0..segs.len()).filter(|i| on(i)).collect();
            // a patch of this node and layer that already holds the point (one node may carry capacitors at several
            // points, such as a port reached at several places)
            let old_patch = patch.get(&(node, layer.clone())).into_iter().flatten().copied().find(|&i| segs[i].rects.iter().any(|r| inside(r, p, tol)));
            let seg = match wires.iter().find(|&&i| segs[i].nodes.contains(&node)).or(wires.first()).copied().or(old_patch) {
                Some(i) => i,
                None => {
                    let q = net_rects(&layer, net);
                    let mut rects: Vec<[i32; 4]> = q.iter().filter(|r| inside(r, p, tol)).cloned().collect();
                    if rects.is_empty() {
                        // a shape within NEAR_NM of the point, else an 18 nm square marks the point itself
                        let near = NEAR_NM * 1e-9 / meters_per_db;
                        if let Some(r) = q.iter().filter(|r| nearest_in(r, p).1 <= near * near).min_by(|a, b| nearest_in(a, p).1.partial_cmp(&nearest_in(b, p).1).unwrap_or(std::cmp::Ordering::Equal)) {
                            rects.push(*r);
                        }
                    }
                    if rects.is_empty() {
                        let h = 9e-9 / meters_per_db;
                        rects.push([(p[0] - h).round() as i32, (p[1] - h).round() as i32, (p[0] + h).round() as i32, (p[1] + h).round() as i32]);
                    }
                    segs.push(Segment { kind: SegKind::Patch, layer: layer.clone(), net, rects, ends: [p, p], res: None, nodes: [node, node] });
                    let i = segs.len() - 1;
                    patch.entry((node, layer.clone())).or_default().push(i);
                    i
                }
            };
            out[s] = Some(CapSide { seg, at: p });
        }
        caps.push(out);
    }
    Segmentation { pos, inferred, from, segs, res, caps }
}

/// length (between its end points) and width (narrowest rectangle side) of a segment in µm
pub fn seg_size_um(s: &Segment, meters_per_db: f64) -> (f64, f64) {
    let d = ((s.ends[0][0] - s.ends[1][0]).powi(2) + (s.ends[0][1] - s.ends[1][1]).powi(2)).sqrt();
    let w = s.rects.iter().map(|r| (r[2] - r[0]).min(r[3] - r[1])).min().unwrap_or(0) as f64;
    (d * meters_per_db * 1e6, w * meters_per_db * 1e6)
}

/// short name of a segment: layer, net and its R (Wire, Via) or the node it is lumped into (Patch)
pub fn seg_label(cell: &LpeCell, seg: &Segmentation, i: usize) -> String {
    let s = &seg.segs[i];
    let net = cell.nets.get(s.net).map(|n| n.name.as_str()).unwrap_or("?");
    let tail = |name: &str| name.rsplit('/').next().unwrap_or(name).to_string();
    match (s.kind, s.res) {
        (SegKind::Patch, _) | (_, None) => format!("{} {} @{}", s.layer, net, tail(&cell.nodes[s.nodes[0]].name)),
        (_, Some(k)) => format!("{} {} {}", s.layer, net, tail(&cell.res[k].name)),
    }
}

#[cfg(test)]
mod segment_tests {
    use super::corridor;

    #[test]
    fn corridor_cuts_a_wire_between_two_points() {
        // a horizontal wire 0..1000 x 0..20 and a vertical stub 400..420 x 0..300
        let rects = [[0, 0, 1000, 20], [400, 0, 420, 300]];
        // between x=100 and x=300 on the wire: the wire cut to 90..310, the stub is outside
        assert_eq!(corridor(&rects, [100.0, 10.0], [300.0, 10.0]), vec![[90, 0, 310, 20]]);
        // from the wire up the stub: the wire around the corner and the stub up to y=200
        let c = corridor(&rects, [410.0, 10.0], [410.0, 200.0]);
        assert!(c.contains(&[400, 0, 420, 210]), "{:?}", c);
    }
}
