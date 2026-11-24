//! Liberty reader (port of the web viewer's liberty.js): keeps library/cell/pin/bus/bundle/ff/latch/
//! statetable/type/pg_pin groups, skips the rest.
use std::collections::HashMap;

#[derive(Debug, Default, Clone)]
pub struct Group {
    pub kind: String,
    pub args: Vec<String>,
    pub attrs: HashMap<String, String>,
    pub groups: Vec<Group>,
}

const KEEP: [&str; 12] = ["library", "cell", "pin", "bus", "bundle", "ff", "latch", "ff_bank", "latch_bank", "statetable", "type", "pg_pin"];

fn is_delim(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'(' | b')' | b'{' | b'}' | b';' | b':' | b',' | b'"' | b'\\')
}

struct P<'a> {
    t: &'a [u8],
    i: usize,
    file: String,
}

impl<'a> P<'a> {
    fn line(&self) -> usize {
        self.t[..self.i.min(self.t.len())].iter().filter(|&&c| c == b'\n').count() + 1
    }
    fn fail(&self, m: &str) -> String {
        format!("{}: line {}: {}", self.file, self.line(), m)
    }
    fn skip_ws(&mut self) {
        let n = self.t.len();
        loop {
            while self.i < n && matches!(self.t[self.i], b' ' | b'\t' | b'\n' | b'\r' | b'\\') {
                self.i += 1;
            }
            if self.i + 1 < n && self.t[self.i] == b'/' && self.t[self.i + 1] == b'*' {
                self.i = memchr::memmem::find(&self.t[self.i + 2..], b"*/").map(|k| self.i + 2 + k + 2).unwrap_or(n);
                continue;
            }
            if self.i + 1 < n && self.t[self.i] == b'/' && self.t[self.i + 1] == b'/' {
                self.i = memchr::memchr(b'\n', &self.t[self.i..]).map(|k| self.i + k + 1).unwrap_or(n);
                continue;
            }
            return;
        }
    }
    fn read_string(&mut self) -> Result<String, String> {
        let n = self.t.len();
        let mut j = self.i + 1;
        let mut out: Vec<u8> = vec![];
        while j < n {
            let d = self.t[j];
            if d == b'"' {
                break;
            }
            if d == b'\\' {
                j += 1;
                if j < n && self.t[j] == b'\r' {
                    j += 1;
                }
                if j < n && self.t[j] == b'\n' {
                    j += 1;
                } else if j < n {
                    out.push(self.t[j]);
                    j += 1;
                }
                continue;
            }
            out.push(d);
            j += 1;
        }
        if j >= n {
            return Err(self.fail("unterminated string"));
        }
        self.i = j + 1;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }
    fn read_word(&mut self) -> String {
        let st = self.i;
        while self.i < self.t.len() && !is_delim(self.t[self.i]) {
            self.i += 1;
        }
        String::from_utf8_lossy(&self.t[st..self.i]).into_owned()
    }
    fn read_value(&mut self) -> Result<String, String> {
        while self.i < self.t.len() && matches!(self.t[self.i], b' ' | b'\t') {
            self.i += 1;
        }
        if self.i < self.t.len() && self.t[self.i] == b'"' {
            return self.read_string();
        }
        let st = self.i;
        while self.i < self.t.len() && !matches!(self.t[self.i], b';' | b'\n' | b'}') {
            self.i += 1;
        }
        Ok(String::from_utf8_lossy(&self.t[st..self.i]).trim().to_string())
    }
    fn read_args(&mut self) -> Result<Vec<String>, String> {
        let mut args = vec![];
        let mut cur = String::new();
        let mut has = false;
        loop {
            self.skip_ws();
            if self.i >= self.t.len() {
                return Err(self.fail("unterminated argument list"));
            }
            let c = self.t[self.i];
            if c == b')' {
                self.i += 1;
                if has {
                    args.push(cur.trim().to_string());
                }
                return Ok(args);
            }
            if c == b',' {
                self.i += 1;
                args.push(cur.trim().to_string());
                cur.clear();
                has = false;
                continue;
            }
            if c == b'"' {
                let s = self.read_string()?;
                if !cur.is_empty() {
                    cur.push(' ');
                }
                cur.push_str(&s);
                has = true;
                continue;
            }
            let w = self.read_word();
            if w.is_empty() {
                cur.push(c as char);
                self.i += 1;
                has = true;
                continue;
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(&w);
            has = true;
        }
    }
    fn skip_group(&mut self) -> Result<(), String> {
        let n = self.t.len();
        let mut d = 1;
        while self.i < n && d > 0 {
            let c = self.t[self.i];
            if c == b'"' {
                self.i += 1;
                while self.i < n {
                    let e = self.t[self.i];
                    if e == b'\\' {
                        self.i += 2;
                        continue;
                    }
                    if e == b'"' {
                        self.i += 1;
                        break;
                    }
                    self.i += 1;
                }
                continue;
            }
            if c == b'/' && self.i + 1 < n && self.t[self.i + 1] == b'*' {
                self.i = memchr::memmem::find(&self.t[self.i + 2..], b"*/").map(|k| self.i + 2 + k + 2).unwrap_or(n);
                continue;
            }
            if c == b'{' {
                d += 1;
            } else if c == b'}' {
                d -= 1;
            }
            self.i += 1;
        }
        if d > 0 {
            return Err(self.fail("unbalanced braces"));
        }
        Ok(())
    }
    fn body(&mut self, node: &mut Group, top: bool) -> Result<(), String> {
        loop {
            self.skip_ws();
            if self.i >= self.t.len() {
                if top {
                    return Ok(());
                }
                return Err(self.fail(&format!("unexpected end of file inside group {}", node.kind)));
            }
            let c = self.t[self.i];
            if c == b'}' {
                if top {
                    return Err(self.fail("unexpected }"));
                }
                self.i += 1;
                return Ok(());
            }
            if c == b';' || c == b',' {
                self.i += 1;
                continue;
            }
            let name = if c == b'"' { self.read_string()? } else { self.read_word() };
            if name.is_empty() {
                return Err(self.fail(&format!("unexpected character '{}'", c as char)));
            }
            self.skip_ws();
            let d = self.t.get(self.i).copied().unwrap_or(0);
            if d == b':' {
                self.i += 1;
                let v = self.read_value()?;
                node.attrs.insert(name, v);
                self.skip_ws();
                if self.t.get(self.i) == Some(&b';') {
                    self.i += 1;
                }
                continue;
            }
            if d == b'(' {
                self.i += 1;
                let args = self.read_args()?;
                self.skip_ws();
                if self.t.get(self.i) == Some(&b'{') {
                    self.i += 1;
                    if KEEP.contains(&name.as_str()) {
                        let mut g = Group { kind: name, args, ..Default::default() };
                        self.body(&mut g, false)?;
                        node.groups.push(g);
                    } else {
                        self.skip_group()?;
                    }
                } else {
                    node.attrs.insert(name, args.join(","));
                    if self.t.get(self.i) == Some(&b';') {
                        self.i += 1;
                    }
                }
                continue;
            }
            if d == b';' {
                self.i += 1;
            }
        }
    }
}

pub fn parse_tree(text: &[u8], file: &str) -> Result<Group, String> {
    let mut p = P { t: text, i: 0, file: file.to_string() };
    let mut root = Group { kind: "root".into(), ..Default::default() };
    p.body(&mut root, true)?;
    Ok(root)
}

#[derive(Debug, Clone, Default)]
pub struct LPin {
    pub name: String,
    pub dir: String,
    pub func: Option<String>,
    pub three_state: Option<String>,
    pub clock: bool,
    pub gate_clock: bool,
}

#[derive(Debug, Clone, Default)]
pub struct State {
    pub clocked_on: Option<String>,
    pub clocked_on_also: Option<String>,
    pub enable: Option<String>,
    pub clear: Option<String>,
    pub preset: Option<String>,
    pub next_state: Option<String>,
    pub data_in: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LCell {
    pub name: String,
    pub pins: Vec<LPin>,
    pub pg_pins: Vec<String>,
    pub ff: Option<State>,
    pub latch: Option<State>,
    pub bank: bool,
    pub statetable: bool,
    pub icg: bool,
}

fn bus_range(t: Option<&Group>) -> Option<(i64, i64)> {
    let t = t?;
    let f = t.attrs.get("bit_from").and_then(|v| v.trim().parse::<i64>().ok());
    let to = t.attrs.get("bit_to").and_then(|v| v.trim().parse::<i64>().ok());
    if let (Some(a), Some(b)) = (f, to) {
        return Some((a, b));
    }
    let w = t.attrs.get("bit_width").and_then(|v| v.trim().parse::<i64>().ok())?;
    Some((w - 1, 0))
}

pub fn cells_from_tree(root: &Group, file: &str) -> Result<Vec<LCell>, String> {
    let libs: Vec<&Group> = root.groups.iter().filter(|g| g.kind == "library").collect();
    if libs.is_empty() {
        return Err(format!("{}: no library group found", file));
    }
    let mut out = vec![];
    for lib in libs {
        let mut types: HashMap<String, &Group> = HashMap::new();
        for g in &lib.groups {
            if g.kind == "type" {
                if let Some(a) = g.args.first() {
                    types.insert(a.clone(), g);
                }
            }
        }
        for cg in lib.groups.iter().filter(|g| g.kind == "cell") {
            let mut cell = LCell { name: cg.args.first().cloned().unwrap_or_default(), icg: cg.attrs.contains_key("clock_gating_integrated_cell"), ..Default::default() };
            let mut ctypes = types.clone();
            for g in &cg.groups {
                if g.kind == "type" {
                    if let Some(a) = g.args.first() {
                        ctypes.insert(a.clone(), g);
                    }
                }
            }
            let mk = |name: &str, a: &HashMap<String, String>| LPin {
                name: name.to_string(),
                dir: a.get("direction").map(|s| s.to_lowercase()).unwrap_or_default(),
                func: a.get("function").cloned(),
                three_state: a.get("three_state").cloned(),
                clock: a.get("clock").map(|v| v == "true").unwrap_or(false),
                gate_clock: a.get("clock_gate_clock_pin").map(|v| v == "true").unwrap_or(false),
            };
            for g in &cg.groups {
                match g.kind.as_str() {
                    "pin" => {
                        for nm in &g.args {
                            cell.pins.push(mk(nm, &g.attrs));
                        }
                    }
                    "pg_pin" => cell.pg_pins.extend(g.args.iter().cloned()),
                    "bus" => {
                        let nm = g.args.first().cloned().unwrap_or_default();
                        let range = bus_range(g.attrs.get("bus_type").and_then(|t| ctypes.get(t).copied()));
                        let mut sub: HashMap<String, &HashMap<String, String>> = HashMap::new();
                        for p in g.groups.iter().filter(|p| p.kind == "pin") {
                            for pn in &p.args {
                                sub.insert(pn.clone(), &p.attrs);
                            }
                        }
                        match range {
                            Some((from, to)) => {
                                let step: i64 = if from >= to { -1 } else { 1 };
                                let mut b = from;
                                loop {
                                    let bit = format!("{}[{}]", nm, b);
                                    let mut a = g.attrs.clone();
                                    if let Some(s) = sub.get(&bit) {
                                        for (k, v) in s.iter() {
                                            a.insert(k.clone(), v.clone());
                                        }
                                    }
                                    cell.pins.push(mk(&bit, &a));
                                    if b == to {
                                        break;
                                    }
                                    b += step;
                                }
                            }
                            None => cell.pins.push(mk(&nm, &g.attrs)),
                        }
                    }
                    "bundle" => {
                        let members: Vec<String> = g.attrs.get("members").map(|m| m.replace(['(', ')', '"'], " ").split(|c: char| c.is_whitespace() || c == ',').filter(|x| !x.is_empty()).map(String::from).collect()).unwrap_or_default();
                        let mut sub: HashMap<String, &HashMap<String, String>> = HashMap::new();
                        for p in g.groups.iter().filter(|p| p.kind == "pin") {
                            for pn in &p.args {
                                sub.insert(pn.clone(), &p.attrs);
                            }
                        }
                        for m in members {
                            let mut a = g.attrs.clone();
                            if let Some(s) = sub.get(&m) {
                                for (k, v) in s.iter() {
                                    a.insert(k.clone(), v.clone());
                                }
                            }
                            cell.pins.push(mk(&m, &a));
                        }
                    }
                    "ff" | "latch" => {
                        let a = &g.attrs;
                        let st = State {
                            next_state: a.get("next_state").cloned(),
                            clocked_on: a.get("clocked_on").cloned(),
                            clocked_on_also: a.get("clocked_on_also").cloned(),
                            data_in: a.get("data_in").cloned(),
                            enable: a.get("enable").cloned(),
                            clear: a.get("clear").cloned(),
                            preset: a.get("preset").cloned(),
                        };
                        if g.kind == "ff" {
                            cell.ff = Some(st);
                        } else {
                            cell.latch = Some(st);
                        }
                    }
                    "ff_bank" | "latch_bank" => cell.bank = true,
                    "statetable" => cell.statetable = true,
                    _ => {}
                }
            }
            out.push(cell);
        }
    }
    Ok(out)
}

pub fn parse_liberty(text: &[u8], file: &str) -> Result<Vec<LCell>, String> {
    cells_from_tree(&parse_tree(text, file)?, file)
}
