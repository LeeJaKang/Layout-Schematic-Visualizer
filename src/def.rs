//! DEF reader (port of the web viewer's createDefParser). The file is read in blocks of whole lines (the
//! whole text is never held); large sections (COMPONENTS, PINS, SPECIALNETS, NETS) are split at item
//! boundaries and read in parallel; the parts are merged in file order, so the result is the same as
//! reading the file from start to end.
use crate::lef::{Lib, MacroShape, Shape};
use crate::tok::{num, s as tstr, Tokens};
use crate::{K_BLOCK, K_IOPIN, K_SVIA, K_SWIRE, K_VIA, K_WIRE};
use rayon::prelude::*;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};

#[derive(Default, Clone)]
pub struct Rects {
    pub r: Vec<[i32; 4]>,
    pub layer: Vec<u8>,
    pub owner: Vec<i32>,
    pub kind: Vec<u8>,
}

/// JavaScript Math.round (half up), used so coordinates match the web viewer exactly
#[inline]
fn jround(v: f64) -> i32 {
    (v + 0.5).floor() as i32
}

impl Rects {
    #[inline]
    pub fn push(&mut self, x1: f64, y1: f64, x2: f64, y2: f64, layer: u8, owner: i32, kind: u8) {
        self.r.push([jround(x1.min(x2)), jround(y1.min(y2)), jround(x1.max(x2)), jround(y1.max(y2))]);
        self.layer.push(layer);
        self.owner.push(owner);
        self.kind.push(kind);
    }
    pub fn len(&self) -> usize {
        self.r.len()
    }
    pub fn is_empty(&self) -> bool {
        self.r.is_empty()
    }
    fn append(&mut self, o: Rects, lmap: &[u8], omap: &dyn Fn(u8, i32) -> i32) {
        self.r.extend_from_slice(&o.r);
        self.layer.extend(o.layer.iter().map(|&l| lmap[l as usize]));
        self.owner.extend(o.owner.iter().zip(o.kind.iter()).map(|(&w, &k)| omap(k, w)));
        self.kind.extend_from_slice(&o.kind);
    }
}

#[derive(Clone, Debug, Default)]
pub struct Comp {
    pub name: String,
    pub model: String,
    pub x: f64,
    pub y: f64,
    pub orient: String,
    pub status: String,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, Default)]
pub struct Net {
    pub name: String,
    pub conns: Vec<(String, String)>,
    pub special: bool,
    pub usage: String,
}

#[derive(Clone, Debug, Default)]
pub struct Port {
    pub shapes: Vec<MacroShape>,
    pub x: f64,
    pub y: f64,
    pub orient: String,
    pub placed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Pin {
    pub name: String,
    pub net: Option<String>,
    pub dir: String,
    pub usage: String,
    pub ports: Vec<Port>,
}

#[derive(Clone, Debug, Default)]
pub struct Row {
    pub name: String,
    pub site: String,
    pub x: f64,
    pub y: f64,
    pub orient: String,
    pub w: f64,
    pub h: f64,
}

#[derive(Clone, Debug, Default)]
pub struct DVia {
    pub shapes: Vec<Shape>, // in DEF units
}

#[derive(Default)]
pub struct Layout {
    pub design: String,
    pub dbu: f64,
    pub die: [f64; 4],
    pub rows: Vec<Row>,
    pub comps: Vec<Comp>,
    pub comp_index: HashMap<String, u32>,
    pub nets: Vec<Net>,
    pub net_index: HashMap<String, u32>,
    pub pins: Vec<Pin>,
    pub rects: Rects,
    pub layer_ids: HashMap<String, u8>,
    pub layer_names: Vec<String>,
    pub vias: HashMap<String, DVia>,
    pub warnings: Vec<String>,
}

pub fn orient_pt(o: &str, x: f64, y: f64, w: f64, h: f64) -> (f64, f64) {
    match o {
        "S" => (w - x, h - y),
        "FN" => (w - x, y),
        "FS" => (x, h - y),
        "W" => (h - y, x),
        "E" => (y, w - x),
        "FW" => (h - y, w - x),
        "FE" => (y, x),
        _ => (x, y),
    }
}
fn swaps_wh(o: &str) -> bool {
    matches!(o, "W" | "E" | "FW" | "FE")
}
fn unesc(t: &[u8]) -> String {
    if !t.contains(&b'\\') {
        return tstr(t);
    }
    let mut out = Vec::with_capacity(t.len());
    let mut i = 0;
    while i < t.len() {
        if t[i] == b'\\' && i + 1 < t.len() {
            out.push(t[i + 1]);
            i += 2;
        } else {
            out.push(t[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// statement scanner over collected tokens
struct S<'a, 'b> {
    t: &'b [&'a [u8]],
    i: usize,
}
impl<'a, 'b> S<'a, 'b> {
    #[inline]
    fn next(&mut self) -> Option<&'a [u8]> {
        let r = self.t.get(self.i).copied();
        self.i += 1;
        r
    }
    #[inline]
    fn peek(&self) -> Option<&'a [u8]> {
        self.t.get(self.i).copied()
    }
    #[inline]
    fn peek2(&self) -> Option<&'a [u8]> {
        self.t.get(self.i + 1).copied()
    }
    #[inline]
    fn num(&mut self) -> f64 {
        self.next().and_then(num).unwrap_or(f64::NAN)
    }
}

/// output of one reader (whole file or one chunk of a section)
#[derive(Default)]
struct Part {
    comps: Vec<Comp>,
    nets: Vec<Net>,
    net_index: HashMap<String, u32>,
    pins: Vec<Pin>,
    rects: Rects,
    layer_names: Vec<String>,
    layer_ids: HashMap<String, u8>,
    warnings: Vec<String>,
}

struct Reader<'l> {
    lib: &'l Lib,
    dvias: &'l HashMap<String, DVia>,
    lvia_cache: HashMap<String, DVia>,
    dbu: f64,
    p: Part,
}

impl<'l> Reader<'l> {
    fn new(lib: &'l Lib, dvias: &'l HashMap<String, DVia>, dbu: f64, seed: &[String]) -> Self {
        let mut p = Part::default();
        for nm in seed {
            let id = p.layer_names.len() as u8;
            p.layer_ids.insert(nm.clone(), id);
            p.layer_names.push(nm.clone());
        }
        Reader { lib, dvias, lvia_cache: HashMap::new(), dbu, p }
    }
    fn warn(&mut self, m: String) {
        if self.p.warnings.len() < 100 {
            self.p.warnings.push(m);
        }
    }
    fn layer_id(&mut self, name: &str) -> u8 {
        if let Some(&id) = self.p.layer_ids.get(name) {
            return id;
        }
        let id = self.p.layer_names.len();
        assert!(id < 255, "more than 255 layers");
        self.p.layer_ids.insert(name.to_string(), id as u8);
        self.p.layer_names.push(name.to_string());
        id as u8
    }
    fn place_via(&mut self, name: &str, x: f64, y: f64, owner: i32, kind: u8) {
        self.place_vias(name, x, y, [1, 1], [0.0, 0.0], owner, kind);
    }
    /// a via, or an array of n[0] x n[1] vias `step` apart (DEF 5.8 `viaName DO numX BY numY STEP stepX stepY`)
    #[allow(clippy::too_many_arguments)]
    fn place_vias(&mut self, name: &str, x: f64, y: f64, n: [u64; 2], step: [f64; 2], owner: i32, kind: u8) {
        let shapes: Vec<Shape> = if let Some(v) = self.dvias.get(name) {
            v.shapes.clone()
        } else if let Some(v) = self.lvia_cache.get(name) {
            v.shapes.clone()
        } else if let Some(lv) = self.lib.vias.get(name) {
            let dbu = self.dbu;
            let conv = DVia { shapes: lv.shapes.iter().map(|sh| Shape { layer: sh.layer.clone(), r: sh.r.map(|z| z * dbu) }).collect() };
            let sh = conv.shapes.clone();
            self.lvia_cache.insert(name.to_string(), conv);
            sh
        } else {
            self.warn(format!("via {} is not defined in the DEF or LEF", name));
            return;
        };
        let ls: Vec<u8> = shapes.iter().map(|sh| self.layer_id(&sh.layer)).collect();
        for j in 0..n[1] {
            let y = if j == 0 { y } else { y + j as f64 * step[1] };
            for i in 0..n[0] {
                let x = if i == 0 { x } else { x + i as f64 * step[0] };
                for (sh, &l) in shapes.iter().zip(&ls) {
                    self.p.rects.push(x + sh.r[0], y + sh.r[1], x + sh.r[2], y + sh.r[3], l, owner, kind);
                }
            }
        }
    }
    fn layer_width(&self, layer: &str) -> f64 {
        self.lib.layers.get(layer).and_then(|l| l.width).map(|w| w * self.dbu).unwrap_or(0.0)
    }

    fn read_routing(&mut self, s: &mut S, owner: i32, special: bool) {
        let mut layer: Option<String> = None;
        let (mut width, mut last_x, mut last_y, mut last_ext, mut have_pt) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, false);
        let (kind_w, kind_v) = if special { (K_SWIRE, K_SVIA) } else { (K_WIRE, K_VIA) };
        let mut first = true;
        loop {
            let t = match s.peek() {
                None | Some(b";") => return,
                Some(t) => t,
            };
            if t == b"+" && !first && !matches!(s.peek2(), Some(b"SHAPE") | Some(b"STYLE") | Some(b"MASK") | Some(b"RECT")) {
                return;
            }
            s.next();
            first = false;
            if t == b"NEW" {
                have_pt = false;
                layer = None;
                continue;
            }
            if t == b"+" {
                match s.next() {
                    Some(b"SHAPE") | Some(b"STYLE") | Some(b"MASK") => {
                        s.next();
                        continue;
                    }
                    Some(b"RECT") => {
                        let ly = tstr(s.next().unwrap_or(b""));
                        s.next();
                        let (x1, y1) = (s.num(), s.num());
                        s.next();
                        s.next();
                        let (x2, y2) = (s.num(), s.num());
                        s.next();
                        let l = self.layer_id(&ly);
                        self.p.rects.push(x1, y1, x2, y2, l, owner, kind_w);
                        continue;
                    }
                    _ => return,
                }
            }
            if layer.is_none() {
                let ly = tstr(t);
                have_pt = false;
                if special {
                    if let Some(w) = s.peek() {
                        if w.first().map(|c| c.is_ascii_digit() || *c == b'-').unwrap_or(false) && (w[0] != b'-' || w.get(1).map(|c| c.is_ascii_digit()).unwrap_or(false)) {
                            width = s.num();
                        }
                    }
                } else {
                    width = self.layer_width(&ly);
                }
                layer = Some(ly);
                continue;
            }
            match t {
                b"TAPER" | b"VIRTUALPIN" => continue,
                b"TAPERRULE" | b"STYLE" | b"MASK" => {
                    s.next();
                    continue;
                }
                b"VIRTUAL" => {
                    s.next();
                    let (x, y) = (s.next().unwrap_or(b"*"), s.next().unwrap_or(b"*"));
                    s.next();
                    if x != b"*" {
                        last_x = num(x).unwrap_or(f64::NAN);
                    }
                    if y != b"*" {
                        last_y = num(y).unwrap_or(f64::NAN);
                    }
                    have_pt = true;
                    continue;
                }
                b"RECT" => {
                    s.next();
                    let (a, b, c, d) = (s.num(), s.num(), s.num(), s.num());
                    s.next();
                    let l = self.layer_id(layer.as_ref().unwrap());
                    self.p.rects.push(last_x + a, last_y + b, last_x + c, last_y + d, l, owner, kind_w);
                    continue;
                }
                b"(" => {
                    let (xs, ys) = (s.next().unwrap_or(b"*"), s.next().unwrap_or(b"*"));
                    let mut ext: Option<f64> = None;
                    let nx = s.next();
                    if nx != Some(b")") {
                        ext = Some(nx.and_then(num).unwrap_or(f64::NAN));
                        s.next();
                    }
                    let x = if xs == b"*" { last_x } else { num(xs).unwrap_or(f64::NAN) };
                    let y = if ys == b"*" { last_y } else { num(ys).unwrap_or(f64::NAN) };
                    let e = ext.unwrap_or(if special { 0.0 } else { width / 2.0 });
                    if have_pt {
                        let hw = width / 2.0;
                        let l = self.layer_id(layer.as_ref().unwrap());
                        if x == last_x {
                            let (lo, lo_e, hi, hi_e) = if y >= last_y { (last_y, last_ext, y, e) } else { (y, e, last_y, last_ext) };
                            self.p.rects.push(x - hw, lo - lo_e, x + hw, hi + hi_e, l, owner, kind_w);
                        } else if y == last_y {
                            let (lo, lo_e, hi, hi_e) = if x >= last_x { (last_x, last_ext, x, e) } else { (x, e, last_x, last_ext) };
                            self.p.rects.push(lo - lo_e, y - hw, hi + hi_e, y + hw, l, owner, kind_w);
                        } else {
                            let m = format!("diagonal wire on {} ignored", layer.as_ref().unwrap());
                            self.warn(m);
                        }
                    }
                    last_x = x;
                    last_y = y;
                    last_ext = e;
                    have_pt = true;
                    continue;
                }
                _ => {
                    if have_pt {
                        if matches!(s.peek(), Some(b"N") | Some(b"S") | Some(b"E") | Some(b"W") | Some(b"FN") | Some(b"FS") | Some(b"FE") | Some(b"FW")) {
                            s.next();
                        }
                        let vn = tstr(t);
                        if s.peek() == Some(b"DO") {
                            // via array: DO numX BY numY STEP stepX stepY, the first via at the current point
                            s.next();
                            let nx = s.num();
                            let by = s.next();
                            let ny = s.num();
                            let st = s.next();
                            let (sx, sy) = (s.num(), s.num());
                            let ok = by == Some(b"BY") && st == Some(b"STEP") && [nx, ny, sx, sy].iter().all(|v| v.is_finite()) && nx >= 1.0 && ny >= 1.0 && nx * ny <= 1e7;
                            if ok {
                                self.place_vias(&vn, last_x, last_y, [nx as u64, ny as u64], [sx, sy], owner, kind_v);
                            } else {
                                self.warn(format!("via array of {} has a bad DO numX BY numY STEP stepX stepY", vn));
                            }
                        } else {
                            self.place_via(&vn, last_x, last_y, owner, kind_v);
                        }
                    }
                }
            }
        }
    }

    fn comp_item(&mut self, s: &mut S) {
        let name = unesc(s.next().unwrap_or(b""));
        let model = tstr(s.next().unwrap_or(b""));
        let mut c = Comp { name, model, orient: "N".into(), status: "UNPLACED".into(), ..Default::default() };
        loop {
            match s.next() {
                None | Some(b";") => break,
                Some(b @ (b"PLACED" | b"FIXED" | b"COVER")) => {
                    c.status = tstr(b);
                    s.next();
                    c.x = s.num();
                    c.y = s.num();
                    s.next();
                    c.orient = tstr(s.next().unwrap_or(b"N"));
                }
                Some(b"UNPLACED") => c.status = "UNPLACED".into(),
                _ => {}
            }
        }
        if let Some(m) = self.lib.macros.get(&c.model) {
            let (w, h) = (m.w * self.dbu, m.h * self.dbu);
            if swaps_wh(&c.orient) {
                c.w = h;
                c.h = w;
            } else {
                c.w = w;
                c.h = h;
            }
        } else {
            let m = format!("component {}: macro {} not in the LEF files", c.name, c.model);
            self.warn(m);
        }
        self.p.comps.push(c);
    }

    fn pin_item(&mut self, s: &mut S) {
        let mut pin = Pin { name: unesc(s.next().unwrap_or(b"")), ..Default::default() };
        let new_port = |pin: &mut Pin| pin.ports.push(Port { orient: "N".into(), ..Default::default() });
        loop {
            match s.next() {
                None | Some(b";") => break,
                Some(b"+") => {}
                _ => continue,
            }
            match s.next() {
                Some(b"NET") => pin.net = Some(unesc(s.next().unwrap_or(b""))),
                Some(b"DIRECTION") => pin.dir = tstr(s.next().unwrap_or(b"")),
                Some(b"USE") => pin.usage = tstr(s.next().unwrap_or(b"")),
                Some(b"PORT") => new_port(&mut pin),
                Some(b"LAYER") => {
                    if pin.ports.is_empty() {
                        new_port(&mut pin);
                    }
                    let ly = tstr(s.next().unwrap_or(b""));
                    let mut c = s.next();
                    while c != Some(b"(") && c.is_some() {
                        if matches!(c, Some(b"MASK") | Some(b"SPACING") | Some(b"DESIGNRULEWIDTH")) {
                            s.next();
                        }
                        c = s.next();
                    }
                    let (x1, y1) = (s.num(), s.num());
                    s.next();
                    s.next();
                    let (x2, y2) = (s.num(), s.num());
                    s.next();
                    pin.ports.last_mut().unwrap().shapes.push(MacroShape::Rect(Shape { layer: ly, r: [x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)] }));
                }
                Some(b"POLYGON") => {
                    if pin.ports.is_empty() {
                        new_port(&mut pin);
                    }
                    let ly = tstr(s.next().unwrap_or(b""));
                    let mut pts = vec![];
                    loop {
                        match s.peek() {
                            None | Some(b"+") | Some(b";") => break,
                            Some(c) => {
                                s.next();
                                if c != b"(" && c != b")" && c != b"MASK" {
                                    pts.push(num(c).unwrap_or(f64::NAN));
                                }
                            }
                        }
                    }
                    let (mut x1, mut y1, mut x2, mut y2) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
                    for (k, v) in pts.iter().enumerate() {
                        if k % 2 == 0 {
                            x1 = x1.min(*v);
                            x2 = x2.max(*v);
                        } else {
                            y1 = y1.min(*v);
                            y2 = y2.max(*v);
                        }
                    }
                    pin.ports.last_mut().unwrap().shapes.push(MacroShape::Rect(Shape { layer: ly, r: [x1, y1, x2, y2] }));
                }
                Some(b"VIA") => {
                    if pin.ports.is_empty() {
                        new_port(&mut pin);
                    }
                    let vn = tstr(s.next().unwrap_or(b""));
                    s.next();
                    let (x, y) = (s.num(), s.num());
                    s.next();
                    pin.ports.last_mut().unwrap().shapes.push(MacroShape::Via { name: vn, at: [x, y] });
                }
                Some(b"PLACED") | Some(b"FIXED") | Some(b"COVER") => {
                    if pin.ports.is_empty() {
                        new_port(&mut pin);
                    }
                    s.next();
                    let (x, y) = (s.num(), s.num());
                    s.next();
                    let o = tstr(s.next().unwrap_or(b"N"));
                    let pt = pin.ports.last_mut().unwrap();
                    pt.x = x;
                    pt.y = y;
                    pt.orient = o;
                    pt.placed = true;
                }
                _ => {}
            }
        }
        let pid = self.p.pins.len() as i32;
        for pt in pin.ports.clone() {
            if !pt.placed {
                continue;
            }
            for sh in &pt.shapes {
                match sh {
                    MacroShape::Via { name, at } => self.place_via(name, pt.x + at[0], pt.y + at[1], pid, K_IOPIN),
                    MacroShape::Rect(r) => {
                        let (ax, ay) = orient_pt(&pt.orient, r.r[0], r.r[1], 0.0, 0.0);
                        let (bx, by) = orient_pt(&pt.orient, r.r[2], r.r[3], 0.0, 0.0);
                        let l = self.layer_id(&r.layer);
                        self.p.rects.push(pt.x + ax.min(bx), pt.y + ay.min(by), pt.x + ax.max(bx), pt.y + ay.max(by), l, pid, K_IOPIN);
                    }
                }
            }
        }
        self.p.pins.push(pin);
    }

    fn blockage_item(&mut self, s: &mut S) {
        let kind = s.next();
        let ly = if kind == Some(b"LAYER") { Some(tstr(s.next().unwrap_or(b""))) } else { None };
        loop {
            match s.next() {
                None | Some(b";") => break,
                Some(b"RECT") => {
                    s.next();
                    let (x1, y1) = (s.num(), s.num());
                    s.next();
                    s.next();
                    let (x2, y2) = (s.num(), s.num());
                    s.next();
                    let l = self.layer_id(ly.as_deref().unwrap_or("(placement)"));
                    self.p.rects.push(x1, y1, x2, y2, l, -1, K_BLOCK);
                }
                _ => {}
            }
        }
    }

    fn net_item(&mut self, s: &mut S, special: bool) {
        let name = unesc(s.next().unwrap_or(b""));
        let net = match self.p.net_index.get(&name) {
            Some(&i) => i,
            None => {
                let i = self.p.nets.len() as u32;
                self.p.nets.push(Net { name: name.clone(), special, ..Default::default() });
                self.p.net_index.insert(name, i);
                i
            }
        };
        if special {
            self.p.nets[net as usize].special = true;
        }
        loop {
            let b = match s.next() {
                None | Some(b";") => break,
                Some(b) => b,
            };
            if b == b"(" {
                let c = unesc(s.next().unwrap_or(b""));
                let p = unesc(s.next().unwrap_or(b""));
                loop {
                    match s.next() {
                        None | Some(b")") => break,
                        _ => {}
                    }
                }
                self.p.nets[net as usize].conns.push((c, p));
                continue;
            }
            if b != b"+" {
                continue;
            }
            match s.next() {
                Some(k @ (b"ROUTED" | b"FIXED" | b"COVER" | b"NOSHIELD" | b"SHIELD")) => {
                    if k == b"SHIELD" {
                        s.next();
                    }
                    self.read_routing(s, net as i32, special);
                }
                Some(b"USE") => self.p.nets[net as usize].usage = tstr(s.next().unwrap_or(b"")),
                Some(b"RECT") => {
                    let ly = tstr(s.next().unwrap_or(b""));
                    s.next();
                    let (x1, y1) = (s.num(), s.num());
                    s.next();
                    s.next();
                    let (x2, y2) = (s.num(), s.num());
                    s.next();
                    let l = self.layer_id(&ly);
                    self.p.rects.push(x1, y1, x2, y2, l, net as i32, if special { K_SWIRE } else { K_WIRE });
                }
                _ => {}
            }
        }
    }

    fn item(&mut self, section: &[u8], toks: &[&[u8]]) {
        let mut s = S { t: toks, i: 1 };
        match section {
            b"COMPONENTS" => self.comp_item(&mut s),
            b"PINS" => self.pin_item(&mut s),
            b"BLOCKAGES" => self.blockage_item(&mut s),
            b"SPECIALNETS" => self.net_item(&mut s, true),
            b"NETS" => self.net_item(&mut s, false),
            _ => {}
        }
    }
}

fn via_item(toks: &[&[u8]], warnings: &mut Vec<String>) -> (String, DVia) {
    let mut s = S { t: toks, i: 1 };
    let name = tstr(s.next().unwrap_or(b""));
    let mut shapes = vec![];
    let (mut rule, mut cut, mut layers, mut sp, mut enc, mut rc, mut origin, mut off) = (false, [0.0; 2], vec![], [0.0; 2], [0.0; 4], [1.0, 1.0], [0.0; 2], [0.0; 4]);
    loop {
        match s.next() {
            None | Some(b";") => break,
            Some(b"+") => {}
            _ => continue,
        }
        match s.next() {
            Some(b"RECT") => {
                let ly = tstr(s.next().unwrap_or(b""));
                s.next();
                let (x1, y1) = (s.num(), s.num());
                s.next();
                s.next();
                let (x2, y2) = (s.num(), s.num());
                s.next();
                shapes.push(Shape { layer: ly, r: [x1.min(x2), y1.min(y2), x1.max(x2), y1.max(y2)] });
            }
            Some(b"POLYGON") => {
                let ly = tstr(s.next().unwrap_or(b""));
                let mut pts = vec![];
                loop {
                    match s.peek() {
                        None | Some(b"+") | Some(b";") => break,
                        Some(c) => {
                            s.next();
                            if c != b"(" && c != b")" {
                                pts.push(num(c).unwrap_or(f64::NAN));
                            }
                        }
                    }
                }
                let (mut x1, mut y1, mut x2, mut y2) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
                for (k, v) in pts.iter().enumerate() {
                    if k % 2 == 0 {
                        x1 = x1.min(*v);
                        x2 = x2.max(*v);
                    } else {
                        y1 = y1.min(*v);
                        y2 = y2.max(*v);
                    }
                }
                shapes.push(Shape { layer: ly, r: [x1, y1, x2, y2] });
            }
            Some(b"VIARULE") => {
                rule = true;
                s.next();
            }
            Some(b"CUTSIZE") => cut = [s.num(), s.num()],
            Some(b"LAYERS") => layers = vec![tstr(s.next().unwrap_or(b"")), tstr(s.next().unwrap_or(b"")), tstr(s.next().unwrap_or(b""))],
            Some(b"CUTSPACING") => sp = [s.num(), s.num()],
            Some(b"ENCLOSURE") => enc = [s.num(), s.num(), s.num(), s.num()],
            Some(b"ROWCOL") => rc = [s.num(), s.num()],
            Some(b"ORIGIN") => origin = [s.num(), s.num()],
            Some(b"OFFSET") => off = [s.num(), s.num(), s.num(), s.num()],
            Some(b"PATTERN") => {
                s.next();
                if warnings.len() < 100 {
                    warnings.push(format!("via {}: cut PATTERN ignored, all cuts drawn", name));
                }
            }
            _ => {}
        }
    }
    if rule && layers.len() == 3 {
        let (cw, ch) = (cut[0], cut[1]);
        let (rows, cols) = (rc[0] as i64, rc[1] as i64);
        let w = cols as f64 * cw + (cols as f64 - 1.0) * sp[0];
        let h = rows as f64 * ch + (rows as f64 - 1.0) * sp[1];
        let (x0, y0) = (-w / 2.0 + origin[0], -h / 2.0 + origin[1]);
        for r in 0..rows {
            for c in 0..cols {
                let (cx, cy) = (x0 + c as f64 * (cw + sp[0]), y0 + r as f64 * (ch + sp[1]));
                shapes.push(Shape { layer: layers[1].clone(), r: [cx, cy, cx + cw, cy + ch] });
            }
        }
        shapes.push(Shape { layer: layers[0].clone(), r: [x0 - enc[0] + off[0], y0 - enc[1] + off[1], x0 + w + enc[0] + off[0], y0 + h + enc[1] + off[1]] });
        shapes.push(Shape { layer: layers[2].clone(), r: [x0 - enc[2] + off[2], y0 - enc[3] + off[3], x0 + w + enc[2] + off[2], y0 + h + enc[3] + off[3]] });
    }
    (name, DVia { shapes })
}

const PARALLEL: [&[u8]; 4] = [b"COMPONENTS", b"PINS", b"SPECIALNETS", b"NETS"];
const DATA: [&[u8]; 6] = [b"VIAS", b"COMPONENTS", b"PINS", b"BLOCKAGES", b"SPECIALNETS", b"NETS"];
const SKIP: [&[u8]; 9] = [b"PROPERTYDEFINITIONS", b"NONDEFAULTRULES", b"REGIONS", b"GROUPS", b"FILLS", b"SCANCHAINS", b"STYLES", b"SLOTS", b"PINPROPERTIES"];

/// tokens of each `- ...;` item in a byte range
fn for_items<'a>(text: &'a [u8], mut f: impl FnMut(&[&'a [u8]])) {
    let mut st = Tokens::new(text);
    let mut stmt: Vec<&[u8]> = Vec::with_capacity(256);
    while let Some(t) = st.next() {
        stmt.push(t);
        if t == b";" {
            if stmt[0] == b"-" {
                f(&stmt);
            }
            stmt.clear();
        }
    }
}

/// split a section body into about `k` pieces at lines that start an item (`- `)
fn split_items(text: &[u8], k: usize) -> Vec<(usize, usize)> {
    let n = text.len();
    let mut cuts = vec![0];
    for i in 1..k {
        let mut p = n * i / k;
        let mut found = None;
        while p < n {
            match memchr::memchr(b'\n', &text[p..]) {
                None => break,
                Some(q) => {
                    let ls = p + q + 1;
                    let mut j = ls;
                    while j < n && (text[j] == b' ' || text[j] == b'\t') {
                        j += 1;
                    }
                    if j + 1 < n && text[j] == b'-' && (text[j + 1] == b' ' || text[j + 1] == b'\t') {
                        found = Some(ls);
                        break;
                    }
                    p = ls;
                }
            }
        }
        if let Some(c) = found {
            if c > *cuts.last().unwrap() {
                cuts.push(c);
            }
        }
    }
    cuts.push(n);
    cuts.windows(2).map(|w| (w[0], w[1])).collect()
}

/// position just after the line `END <name>` that ends a section starting at `from`
fn find_end(text: &[u8], from: usize, name: &[u8]) -> Option<(usize, usize)> {
    let mut pat = b"END ".to_vec();
    pat.extend_from_slice(name);
    let mut p = from;
    while let Some(q) = memchr::memmem::find(&text[p..], &pat) {
        let at = p + q;
        let line_start = at == 0 || text[..at].iter().rev().take_while(|&&c| c != b'\n').all(|&c| c == b' ' || c == b'\t');
        let after = at + pat.len();
        let word_end = after >= text.len() || matches!(text[after], b' ' | b'\t' | b'\n' | b'\r');
        if line_start && word_end {
            return Some((at, after));
        }
        p = at + 1;
    }
    None
}

/// bytes read per block when streaming a DEF; blocks end at a line end
const BLOCK: usize = 64 << 20;

/// open a DEF file, plain or gzip, as a byte stream
pub fn open_def(path: &Path) -> std::io::Result<Box<dyn Read + Send>> {
    let mut f = File::open(path)?;
    let mut head = [0u8; 2];
    let n = f.read(&mut head)?;
    f.seek(SeekFrom::Start(0))?;
    if n == 2 && head == [0x1f, 0x8b] {
        Ok(Box::new(flate2::read::MultiGzDecoder::new(BufReader::with_capacity(1 << 20, f))))
    } else {
        Ok(Box::new(f))
    }
}

/// read a DEF file block by block (the whole text is never held); returns the layout and the text size in bytes
pub fn parse_def_file(path: &Path, lib: &Lib, threads: usize) -> std::io::Result<(Layout, u64)> {
    parse_def_stream(open_def(path)?, &path.display().to_string(), lib, threads)
}

pub fn parse_def(text: &[u8], file: &str, lib: &Lib, threads: usize) -> Layout {
    parse_def_stream(text, file, lib, threads).map(|r| r.0).unwrap_or_else(|e| Layout { warnings: vec![format!("{}: {}", file, e)], ..Default::default() })
}

/// reader thread: sends blocks of about BLOCK bytes that end at a newline (the last one may not)
fn read_blocks<R: Read>(mut src: R, tx: SyncSender<std::io::Result<Vec<u8>>>, total: &AtomicU64) {
    // test hook: LSV_DEF_BLOCK=bytes makes blocks small so that block ends fall everywhere
    let block = std::env::var("LSV_DEF_BLOCK").ok().and_then(|v| v.parse::<usize>().ok()).filter(|&v| v > 0).unwrap_or(BLOCK);
    let mut carry: Vec<u8> = vec![];
    loop {
        let mut b = std::mem::take(&mut carry);
        let start = b.len();
        b.resize(start + block, 0);
        let (mut n, mut eof) = (start, false);
        while n < b.len() {
            match src.read(&mut b[n..]) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(k) => n += k,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    let _ = tx.send(Err(e));
                    return;
                }
            }
        }
        b.truncate(n);
        total.fetch_add((n - start) as u64, Ordering::Relaxed);
        if !eof {
            match memchr::memrchr(b'\n', &b) {
                Some(p) => carry = b.split_off(p + 1),
                None => {
                    // one line longer than a block: keep reading
                    carry = b;
                    continue;
                }
            }
        }
        if tx.send(Ok(b)).is_err() || eof {
            return;
        }
    }
}

/// the part of the file in memory: whole lines, refilled from the reader thread
struct Win {
    buf: Vec<u8>,
    eof: bool,
    rx: Receiver<std::io::Result<Vec<u8>>>,
    err: Option<std::io::Error>,
}
impl Win {
    /// drop buf[..keep] and append the next block; sets eof when the file is exhausted
    fn more(&mut self, keep: usize) {
        self.buf.drain(..keep);
        match self.rx.recv() {
            Ok(Ok(b)) => {
                if self.buf.is_empty() {
                    self.buf = b;
                } else {
                    self.buf.extend_from_slice(&b);
                }
            }
            Ok(Err(e)) => {
                self.err = Some(e);
                self.eof = true;
            }
            Err(_) => self.eof = true,
        }
    }
}

/// state of the statement scan between windows
struct Top {
    section: Option<&'static [u8]>,
    in_ext: bool,
    pending_end: bool,
    die: Option<[f64; 4]>,
}

enum Scan {
    /// the window ended: keep the text from this position and read more
    More(usize),
    /// a large section starts: its name and where its body starts
    Section(&'static [u8], usize),
    Done,
}

fn known(head: &[u8]) -> Option<&'static [u8]> {
    PARALLEL.iter().chain(DATA.iter()).chain(SKIP.iter()).find(|&&k| k == head).copied()
}

/// position of the last line in text[from..] that starts an item (`- `), or `from`
fn last_item_line(text: &[u8], from: usize) -> usize {
    let mut end = text.len();
    while let Some(q) = memchr::memrchr(b'\n', &text[from..end]) {
        let ls = from + q + 1;
        let mut j = ls;
        while j < text.len() && (text[j] == b' ' || text[j] == b'\t') {
            j += 1;
        }
        if j + 1 < text.len() && text[j] == b'-' && (text[j + 1] == b' ' || text[j + 1] == b'\t') {
            return ls;
        }
        end = from + q;
    }
    from
}

/// read the items of one piece of a large section body in parallel; the parts are merged later in file order
fn parse_parts(lib: &Lib, vias: &HashMap<String, DVia>, names: &[String], dbu: f64, head: &[u8], body: &[u8], threads: usize) -> Vec<Part> {
    let k = if body.len() > (8 << 20) { threads.max(1) * 4 } else { 1 };
    let pieces = split_items(body, k);
    pieces
        .par_iter()
        .map(|&(a, b)| {
            let mut r = Reader::new(lib, vias, dbu, names);
            for_items(&body[a..b], |toks| r.item(head, toks));
            r.p
        })
        .collect()
}

fn merge_parts(lay: &mut Layout, parts: Vec<Part>) {
    for p in parts {
        merge(lay, p);
    }
}

/// parse a DEF from a byte stream; a reader thread unpacks the next blocks while this one parses
pub fn parse_def_stream<R: Read + Send>(src: R, file: &str, lib: &Lib, threads: usize) -> std::io::Result<(Layout, u64)> {
    let mut lay = Layout { dbu: 1000.0, ..Default::default() };
    for nm in &lib.layer_order {
        let id = lay.layer_names.len() as u8;
        lay.layer_ids.insert(nm.clone(), id);
        lay.layer_names.push(nm.clone());
    }
    let total = AtomicU64::new(0);
    let mut top = Top { section: None, in_ext: false, pending_end: false, die: None };
    let err = std::thread::scope(|sc| {
        let (tx, rx) = sync_channel(2);
        let tot = &total;
        sc.spawn(move || read_blocks(src, tx, tot));
        let mut win = Win { buf: vec![], eof: false, rx, err: None };
        let mut pos = 0;
        let mut tm = [0.0f64; 3];
        loop {
            match scan(&win.buf, pos, win.eof, &mut top, &mut lay, lib, file) {
                Scan::Done => break,
                Scan::More(keep) => {
                    let t = std::time::Instant::now();
                    win.more(keep);
                    tm[0] += t.elapsed().as_secs_f64();
                    pos = 0;
                }
                Scan::Section(head, body_start) => {
                    // the body is read block by block up to its END line; one block is merged while the next is parsed
                    let t = std::time::Instant::now();
                    let vias = std::mem::take(&mut lay.vias);
                    let mut pending: Vec<Part> = vec![];
                    let mut from = body_start;
                    pos = loop {
                        let end = find_end(&win.buf, from, head);
                        if end.is_none() && win.eof {
                            merge_parts(&mut lay, std::mem::take(&mut pending));
                            lay.warnings.push(format!("{}: section {} has no END line", file, tstr(head)));
                            top.section = Some(head);
                            break from;
                        }
                        let cut = match end {
                            Some((end_at, _)) => end_at,
                            None => last_item_line(&win.buf, from),
                        };
                        if cut > from || !pending.is_empty() {
                            let (names, dbu, body, prev) = (lay.layer_names.clone(), lay.dbu, &win.buf[from..cut], std::mem::take(&mut pending));
                            let lay_m = &mut lay;
                            pending = rayon::join(|| merge_parts(lay_m, prev), || parse_parts(lib, &vias, &names, dbu, head, body, threads)).1;
                        }
                        if let Some((_, after)) = end {
                            let t2 = std::time::Instant::now();
                            merge_parts(&mut lay, std::mem::take(&mut pending));
                            tm[2] += t2.elapsed().as_secs_f64();
                            break after;
                        }
                        let t1 = std::time::Instant::now();
                        win.more(cut);
                        tm[0] += t1.elapsed().as_secs_f64();
                        from = 0;
                    };
                    lay.vias = vias;
                    tm[1] += t.elapsed().as_secs_f64();
                }
            }
            if win.err.is_some() {
                break;
            }
        }
        // test hook: LSV_DEF_TIMING=1 prints where the time went
        if std::env::var("LSV_DEF_TIMING").is_ok() {
            eprintln!("def timing: large sections {:.2} s (of it waiting for text {:.2} s, last merge {:.2} s)", tm[1], tm[0], tm[2]);
        }
        win.err.take()
    });
    if let Some(e) = err {
        return Err(e);
    }
    set_die(&mut lay, top.die);
    Ok((lay, total.load(Ordering::Relaxed)))
}

/// statements of the window from `from`: header lines, small sections in place, and the start of a large section
fn scan(text: &[u8], from: usize, eof: bool, top: &mut Top, lay: &mut Layout, lib: &Lib, file: &str) -> Scan {
    let mut st = Tokens::new(text);
    st.pos = from;
    let mut stmt: Vec<&[u8]> = Vec::with_capacity(256);
    let mut stmt_start = from;
    // items of small data sections (VIAS, BLOCKAGES) are read in place
    loop {
        let before = st.pos;
        let t = match st.next() {
            None if !eof => return Scan::More(if stmt.is_empty() { text.len() } else { stmt_start }),
            None => break,
            Some(t) => t,
        };
        if !eof && st.pos >= text.len() {
            // only a quoted string can run into the window end: read it again with more text
            return Scan::More(if stmt.is_empty() { before } else { stmt_start });
        }
        if top.in_ext {
            if t == b"ENDEXT" {
                top.in_ext = false;
            }
            continue;
        }
        if top.pending_end {
            top.pending_end = false;
            if Some(t) == top.section {
                top.section = None;
            }
            continue;
        }
        if stmt.is_empty() {
            if t == b"END" {
                top.pending_end = true;
                continue;
            }
            if t == b"BEGINEXT" {
                top.in_ext = true;
                continue;
            }
            stmt_start = before;
        }
        stmt.push(t);
        if t != b";" {
            continue;
        }
        let toks = std::mem::take(&mut stmt);
        if let Some(sec) = top.section {
            if toks[0] == b"-" {
                if sec == b"VIAS" {
                    let (nm, v) = via_item(&toks, &mut lay.warnings);
                    lay.vias.insert(nm, v);
                } else {
                    // BLOCKAGES, and any section whose END line was not found
                    let p = {
                        let mut r = Reader::new(lib, &lay.vias, lay.dbu, &lay.layer_names);
                        r.item(sec, &toks);
                        r.p
                    };
                    merge(lay, p);
                }
            }
            continue;
        }
        let head = toks[0];
        let mut s = S { t: &toks, i: 1 };
        match head {
            b"DESIGN" => lay.design = tstr(s.next().unwrap_or(b"")),
            b"UNITS" => {
                s.next();
                s.next();
                lay.dbu = s.num();
            }
            b"DIEAREA" => {
                let mut pts = vec![];
                for a in &toks[1..] {
                    if *a != b"(" && *a != b")" && *a != b";" {
                        pts.push(num(a).unwrap_or(f64::NAN));
                    }
                }
                let (mut x1, mut y1, mut x2, mut y2) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
                for (k, v) in pts.iter().enumerate() {
                    if k % 2 == 0 {
                        x1 = x1.min(*v);
                        x2 = x2.max(*v);
                    } else {
                        y1 = y1.min(*v);
                        y2 = y2.max(*v);
                    }
                }
                top.die = Some([x1, y1, x2, y2]);
            }
            b"ROW" => {
                let name = tstr(s.next().unwrap_or(b""));
                let site = tstr(s.next().unwrap_or(b""));
                let (x, y) = (s.num(), s.num());
                let orient = tstr(s.next().unwrap_or(b""));
                let (mut nx, mut ny, mut sx, mut sy) = (1.0, 1.0, 0.0, 0.0);
                loop {
                    match s.next() {
                        None | Some(b";") => break,
                        Some(b"DO") => {
                            nx = s.num();
                            s.next();
                            ny = s.num();
                        }
                        Some(b"STEP") => {
                            sx = s.num();
                            sy = s.num();
                        }
                        _ => {}
                    }
                }
                let (sw, sh) = match lib.sites.get(&site) {
                    Some(sz) => (sz[0] * lay.dbu, sz[1] * lay.dbu),
                    None => (sx, 0.0),
                };
                let _ = sy;
                lay.rows.push(Row { name, site, x, y, orient, w: if nx > 1.0 { (nx - 1.0) * sx + sw } else { sw }, h: if ny > 1.0 { (ny - 1.0) * sy + sh } else { sh } });
            }
            _ => {
                if PARALLEL.contains(&head) {
                    // the caller reads the section body in parallel, then continues after its END line
                    return Scan::Section(known(head).unwrap(), st.pos);
                } else if DATA.contains(&head) || SKIP.contains(&head) {
                    top.section = known(head);
                }
            }
        }
    }
    if !stmt.is_empty() {
        lay.warnings.push(format!("{}: the file ends inside a statement starting with '{}'", file, tstr(stmt[0])));
    }
    Scan::Done
}

/// die area: DIEAREA, else the box around the components
fn set_die(lay: &mut Layout, die: Option<[f64; 4]>) {
    lay.die = die.unwrap_or_else(|| {
        let (mut x1, mut y1, mut x2, mut y2) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for c in &lay.comps {
            x1 = x1.min(c.x);
            y1 = y1.min(c.y);
            x2 = x2.max(c.x + c.w);
            y2 = y2.max(c.y + c.h);
        }
        if x1.is_finite() { [x1, y1, x2, y2] } else { [0.0, 0.0, 1000.0, 1000.0] }
    });
}

/// append one part in file order: renumber layers, nets (merging by name), pins and components
fn merge(lay: &mut Layout, p: Part) {
    let mut lmap = Vec::with_capacity(p.layer_names.len());
    for nm in &p.layer_names {
        let id = match lay.layer_ids.get(nm) {
            Some(&id) => id,
            None => {
                let id = lay.layer_names.len() as u8;
                lay.layer_ids.insert(nm.clone(), id);
                lay.layer_names.push(nm.clone());
                id
            }
        };
        lmap.push(id);
    }
    let mut nmap = Vec::with_capacity(p.nets.len());
    for net in p.nets {
        match lay.net_index.get(&net.name) {
            Some(&g) => {
                let gn = &mut lay.nets[g as usize];
                gn.special |= net.special;
                gn.conns.extend(net.conns);
                if !net.usage.is_empty() {
                    gn.usage = net.usage;
                }
                nmap.push(g as i32);
            }
            None => {
                let g = lay.nets.len() as u32;
                lay.net_index.insert(net.name.clone(), g);
                lay.nets.push(net);
                nmap.push(g as i32);
            }
        }
    }
    let pin_off = lay.pins.len() as i32;
    let omap = |k: u8, w: i32| -> i32 {
        if w < 0 {
            return w;
        }
        match k {
            K_WIRE | K_VIA | K_SWIRE | K_SVIA => nmap[w as usize],
            K_IOPIN => w + pin_off,
            _ => w,
        }
    };
    lay.rects.append(p.rects, &lmap, &omap);
    for c in p.comps {
        lay.comp_index.insert(c.name.clone(), lay.comps.len() as u32);
        lay.comps.push(c);
    }
    lay.pins.extend(p.pins);
    for w in p.warnings {
        if lay.warnings.len() < 100 {
            lay.warnings.push(w);
        }
    }
}

/// macro shapes of one component in DEF coordinates: (layer, pin name or None for obstructions, rect)
pub fn comp_shapes(lay: &Layout, lib: &Lib, c: &Comp) -> Vec<(String, Option<String>, [f64; 4])> {
    let m = match lib.macros.get(&c.model) {
        Some(m) => m,
        None => return vec![],
    };
    let dbu = lay.dbu;
    let (w, h, ox, oy) = (m.w * dbu, m.h * dbu, m.origin[0] * dbu, m.origin[1] * dbu);
    let mut out = vec![];
    let mut add = |layer: &str, r: [f64; 4], pin: Option<&String>| {
        let (ax, ay) = orient_pt(&c.orient, r[0] * dbu + ox, r[1] * dbu + oy, w, h);
        let (bx, by) = orient_pt(&c.orient, r[2] * dbu + ox, r[3] * dbu + oy, w, h);
        out.push((layer.to_string(), pin.cloned(), [c.x + ax.min(bx), c.y + ay.min(by), c.x + ax.max(bx), c.y + ay.max(by)]));
    };
    let mut one = |sh: &MacroShape, pin: Option<&String>| match sh {
        MacroShape::Rect(r) => add(&r.layer, r.r, pin),
        MacroShape::Via { name, at } => {
            let shapes: Vec<Shape> = if let Some(v) = lay.vias.get(name) {
                v.shapes.iter().map(|q| Shape { layer: q.layer.clone(), r: q.r.map(|z| z / dbu) }).collect()
            } else if let Some(v) = lib.vias.get(name) {
                v.shapes.clone()
            } else {
                vec![]
            };
            for q in shapes {
                add(&q.layer, [q.r[0] + at[0], q.r[1] + at[1], q.r[2] + at[0], q.r[3] + at[1]], pin);
            }
        }
    };
    for p in &m.pins {
        for sh in &p.shapes {
            one(sh, Some(&p.name));
        }
    }
    for sh in &m.obs {
        one(sh, None);
    }
    out
}
