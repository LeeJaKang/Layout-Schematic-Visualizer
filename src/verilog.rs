//! Structural Verilog reader (port of the web viewer's verilog.js): modules, declarations, assign, instances.
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq)]
enum T {
    Id(String, bool), // name, escaped
    Num(String),
    Str,
    P(char),
}

#[derive(Debug, Clone)]
struct Tok {
    t: T,
    line: usize,
}

fn lex(src: &[u8]) -> Vec<Tok> {
    let n = src.len();
    let mut out = Vec::with_capacity(n / 6);
    let (mut i, mut line) = (0usize, 1usize);
    let s = |a: usize, b: usize| String::from_utf8_lossy(&src[a..b]).into_owned();
    while i < n {
        let c = src[i];
        if c == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if matches!(c, b' ' | b'\t' | b'\r' | 0x0c) {
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < n && src[i + 1] == b'/' {
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < n && src[i + 1] == b'*' {
            let end = memchr::memmem::find(&src[i + 2..], b"*/").map(|k| i + 2 + k + 2).unwrap_or(n);
            line += src[i..end].iter().filter(|&&x| x == b'\n').count();
            i = end;
            continue;
        }
        if c == b'(' && i + 2 < n && src[i + 1] == b'*' && src[i + 2] != b')' {
            let end = memchr::memmem::find(&src[i + 2..], b"*)").map(|k| i + 2 + k + 2).unwrap_or(n);
            line += src[i..end].iter().filter(|&&x| x == b'\n').count();
            i = end;
            continue;
        }
        if c == b'`' {
            while i < n && src[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'\\' {
            let mut j = i + 1;
            while j < n && !src[j].is_ascii_whitespace() {
                j += 1;
            }
            out.push(Tok { t: T::Id(s(i + 1, j), true), line });
            i = j;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = i + 1;
            while j < n && (src[j].is_ascii_alphanumeric() || src[j] == b'_' || src[j] == b'$') {
                j += 1;
            }
            out.push(Tok { t: T::Id(s(i, j), false), line });
            i = j;
            continue;
        }
        if c.is_ascii_digit() || c == b'\'' {
            if let Some(len) = number_len(&src[i..(i + 200).min(n)]) {
                out.push(Tok { t: T::Num(s(i, i + len)), line });
                i += len;
                continue;
            }
        }
        if c == b'"' {
            let mut j = i + 1;
            while j < n && src[j] != b'"' {
                if src[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            out.push(Tok { t: T::Str, line });
            i = j + 1;
            continue;
        }
        out.push(Tok { t: T::P(c as char), line });
        i += 1;
    }
    out
}

/// length of a number at the start of `t`: sized/based literal or plain decimal
fn number_len(t: &[u8]) -> Option<usize> {
    let mut i = 0;
    let n = t.len();
    let digits = |i: &mut usize| {
        let st = *i;
        if *i < n && t[*i].is_ascii_digit() {
            *i += 1;
            while *i < n && (t[*i].is_ascii_digit() || t[*i] == b'_') {
                *i += 1;
            }
        }
        *i > st
    };
    let had = digits(&mut i);
    let save = i;
    let mut j = i;
    while j < n && t[j].is_ascii_whitespace() {
        j += 1;
    }
    if j < n && t[j] == b'\'' {
        j += 1;
        while j < n && t[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < n && (t[j] == b's' || t[j] == b'S') {
            j += 1;
        }
        while j < n && t[j].is_ascii_whitespace() {
            j += 1;
        }
        if j < n && b"bBoOdDhH".contains(&t[j]) {
            j += 1;
            while j < n && t[j].is_ascii_whitespace() {
                j += 1;
            }
            let st = j;
            while j < n && (t[j].is_ascii_hexdigit() || b"xXzZ_?".contains(&t[j])) {
                j += 1;
            }
            if j > st {
                return Some(j);
            }
        }
    }
    if had {
        Some(save)
    } else {
        None
    }
}

/// bits of a number, MSB first, each b'0' b'1' b'x' b'z'
pub fn number_bits(text: &str) -> Result<Vec<u8>, String> {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let (width, base, digits) = match t.find('\'') {
        Some(q) => {
            let w = if q > 0 { t[..q].replace('_', "").parse::<usize>().unwrap_or(32) } else { 32 };
            let mut rest = &t[q + 1..];
            if rest.starts_with('s') || rest.starts_with('S') {
                rest = &rest[1..];
            }
            let b = rest.chars().next().unwrap_or('d').to_ascii_lowercase();
            (w, b, rest[1..].replace('_', "").to_lowercase())
        }
        None => (32, 'd', t.replace('_', "")),
    };
    let mut bits: Vec<u8> = vec![];
    if base == 'd' {
        if digits.contains(['x', 'z', '?']) {
            bits = vec![if digits.contains('z') || digits.contains('?') { b'z' } else { b'x' }; width];
        } else {
            let mut v: u128 = digits.parse::<u128>().unwrap_or(0);
            let mut tmp = vec![];
            for _ in 0..width {
                tmp.push(if v & 1 == 1 { b'1' } else { b'0' });
                v >>= 1;
            }
            tmp.reverse();
            bits = tmp;
        }
    } else {
        let per = match base {
            'b' => 1,
            'o' => 3,
            _ => 4,
        };
        let radix = match base {
            'b' => 2,
            'o' => 8,
            _ => 16,
        };
        for d in digits.chars() {
            if d == 'x' || d == 'z' || d == '?' {
                for _ in 0..per {
                    bits.push(if d == '?' { b'z' } else { d as u8 });
                }
                continue;
            }
            let v = d.to_digit(radix).ok_or_else(|| format!("bad number {}", text))?;
            for k in (0..per).rev() {
                bits.push(if (v >> k) & 1 == 1 { b'1' } else { b'0' });
            }
        }
        if bits.len() > width {
            bits = bits[bits.len() - width..].to_vec();
        }
        while bits.len() < width {
            let f = bits.first().copied().unwrap_or(b'0');
            bits.insert(0, if f == b'x' || f == b'z' { f } else { b'0' });
        }
    }
    Ok(bits)
}

#[derive(Debug, Clone)]
pub enum Expr {
    Const(Vec<u8>),
    Concat(Vec<Expr>),
    Repl(usize, Vec<Expr>),
    Id { name: String, idx: Option<i64>, range: Option<(i64, i64)> },
}

#[derive(Debug, Clone, Default)]
pub struct Decl {
    pub dir: Option<String>,
    pub msb: Option<i64>,
    pub lsb: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct Inst {
    pub typ: String,
    pub name: String,
    pub prim: bool,
    pub named: Option<Vec<(String, Option<Expr>)>>,
    pub positional: Option<Vec<Option<Expr>>>,
    pub line: usize,
}

#[derive(Debug, Clone)]
pub struct Assign {
    pub lhs: Expr,
    pub rhs: Expr,
    pub line: usize,
}

#[derive(Debug, Clone, Default)]
pub struct Module {
    pub name: String,
    pub ports: Vec<String>,
    pub decl_order: Vec<String>,
    pub decls: HashMap<String, Decl>,
    pub assigns: Vec<Assign>,
    pub insts: Vec<Inst>,
}

impl Module {
    pub fn declare(&mut self, name: &str, dir: Option<String>, rng: Option<(i64, i64)>) {
        if !self.decls.contains_key(name) {
            self.decl_order.push(name.to_string());
            self.decls.insert(name.to_string(), Decl::default());
        }
        let d = self.decls.get_mut(name).unwrap();
        if dir.is_some() {
            d.dir = dir;
        }
        if let Some((a, b)) = rng {
            d.msb = Some(a);
            d.lsb = Some(b);
        }
    }
}

pub const PRIMS: [&str; 8] = ["and", "nand", "or", "nor", "xor", "xnor", "buf", "not"];

struct Ps<'a> {
    t: &'a [Tok],
    p: usize,
    file: &'a str,
}

impl<'a> Ps<'a> {
    fn fail<X>(&self, m: &str) -> Result<X, String> {
        let line = self.t.get(self.p.min(self.t.len().saturating_sub(1))).map(|x| x.line).unwrap_or(0);
        Err(format!("{}: line {}: {}", self.file, line, m))
    }
    fn is_p(&self, c: char, k: usize) -> bool {
        matches!(self.t.get(self.p + k), Some(Tok { t: T::P(x), .. }) if *x == c)
    }
    fn is_id(&self, k: usize) -> bool {
        matches!(self.t.get(self.p + k), Some(Tok { t: T::Id(..), .. }))
    }
    fn is_num(&self, k: usize) -> bool {
        matches!(self.t.get(self.p + k), Some(Tok { t: T::Num(..), .. }))
    }
    fn is_kw(&self, v: &str) -> bool {
        matches!(self.t.get(self.p), Some(Tok { t: T::Id(x, false), .. }) if x == v)
    }
    fn show(&self) -> String {
        match self.t.get(self.p) {
            None => "end of file".into(),
            Some(Tok { t: T::Id(v, _), .. }) | Some(Tok { t: T::Num(v), .. }) => v.clone(),
            Some(Tok { t: T::P(c), .. }) => c.to_string(),
            Some(Tok { t: T::Str, .. }) => "string".into(),
        }
    }
    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.is_p(c, 0) {
            self.p += 1;
            Ok(())
        } else {
            let s = self.show();
            self.fail(&format!("expected '{}' but found '{}'", c, s))
        }
    }
    fn expect_id(&mut self) -> Result<String, String> {
        match self.t.get(self.p) {
            Some(Tok { t: T::Id(v, _), .. }) => {
                self.p += 1;
                Ok(v.clone())
            }
            _ => {
                let s = self.show();
                self.fail(&format!("expected a name but found '{}'", s))
            }
        }
    }
    fn skip_balanced(&mut self, open: char, close: char) -> Result<(), String> {
        let mut d = 0i32;
        loop {
            match self.t.get(self.p) {
                None => return self.fail(&format!("unbalanced {}", open)),
                Some(Tok { t: T::P(c), .. }) => {
                    if *c == open {
                        d += 1;
                    } else if *c == close {
                        d -= 1;
                    }
                }
                _ => {}
            }
            self.p += 1;
            if d <= 0 {
                return Ok(());
            }
        }
    }
    fn skip_to_semi(&mut self) -> Result<(), String> {
        while self.p < self.t.len() && !self.is_p(';', 0) {
            if self.is_p('(', 0) {
                self.skip_balanced('(', ')')?;
            } else if self.is_p('{', 0) {
                self.skip_balanced('{', '}')?;
            } else {
                self.p += 1;
            }
        }
        self.p += 1;
        Ok(())
    }
    fn const_int(&mut self) -> Result<i64, String> {
        match self.t.get(self.p) {
            Some(Tok { t: T::Num(v), .. }) => {
                self.p += 1;
                let b = number_bits(v)?;
                let s: String = b.iter().map(|&c| if c == b'1' { '1' } else { '0' }).collect();
                Ok(i64::from_str_radix(s.trim_start_matches('0'), 2).unwrap_or(0))
            }
            _ => self.fail("expected a number (parameters in ranges are not supported)"),
        }
    }
    fn range(&mut self) -> Result<(i64, i64), String> {
        self.expect('[')?;
        let msb = self.const_int()?;
        let mut lsb = msb;
        if self.is_p(':', 0) {
            self.p += 1;
            lsb = self.const_int()?;
        }
        self.expect(']')?;
        Ok((msb, lsb))
    }
    fn expr(&mut self) -> Result<Expr, String> {
        if self.is_p('{', 0) {
            self.p += 1;
            if self.is_num(0) && self.is_p('{', 1) {
                let count = self.const_int()? as usize;
                self.expect('{')?;
                let mut parts = vec![self.expr()?];
                while self.is_p(',', 0) {
                    self.p += 1;
                    parts.push(self.expr()?);
                }
                self.expect('}')?;
                self.expect('}')?;
                return Ok(Expr::Repl(count, parts));
            }
            let mut parts = vec![self.expr()?];
            while self.is_p(',', 0) {
                self.p += 1;
                parts.push(self.expr()?);
            }
            self.expect('}')?;
            return Ok(Expr::Concat(parts));
        }
        if let Some(Tok { t: T::Num(v), .. }) = self.t.get(self.p) {
            self.p += 1;
            return Ok(Expr::Const(number_bits(v)?));
        }
        if self.is_id(0) {
            let name = self.expect_id()?;
            if self.is_p('[', 0) {
                let (msb, lsb) = self.range()?;
                return Ok(if msb == lsb { Expr::Id { name, idx: Some(msb), range: None } } else { Expr::Id { name, idx: None, range: Some((msb, lsb)) } });
            }
            return Ok(Expr::Id { name, idx: None, range: None });
        }
        let s = self.show();
        self.fail(&format!("unsupported expression starting with '{}' (only nets, bit selects, concatenations and constants are read)", s))
    }

    fn module(&mut self) -> Result<Module, String> {
        let mut m = Module { name: self.expect_id()?, ..Default::default() };
        if self.is_p('#', 0) {
            self.p += 1;
            self.skip_balanced('(', ')')?;
        }
        if self.is_p('(', 0) {
            self.p += 1;
            let (mut dir, mut rng): (Option<String>, Option<(i64, i64)>) = (None, None);
            while !self.is_p(')', 0) {
                if self.p >= self.t.len() {
                    return self.fail("unexpected end of file in the module header");
                }
                if self.is_p(',', 0) {
                    self.p += 1;
                    continue;
                }
                if self.is_kw("input") || self.is_kw("output") || self.is_kw("inout") {
                    dir = Some(self.expect_id()?);
                    rng = None;
                    continue;
                }
                if self.is_kw("wire") || self.is_kw("reg") || self.is_kw("logic") || self.is_kw("signed") || self.is_kw("tri") {
                    self.p += 1;
                    continue;
                }
                if self.is_p('[', 0) {
                    rng = Some(self.range()?);
                    continue;
                }
                if self.is_p('.', 0) {
                    return self.fail("named port expressions in a module header are not supported");
                }
                let nm = self.expect_id()?;
                m.ports.push(nm.clone());
                if dir.is_some() {
                    m.declare(&nm, dir.clone(), rng);
                }
            }
            self.p += 1;
        }
        self.expect(';')?;
        while self.p < self.t.len() && !self.is_kw("endmodule") {
            let tok = self.t[self.p].clone();
            let (v, esc) = match &tok.t {
                T::P(';') => {
                    self.p += 1;
                    continue;
                }
                T::Id(v, e) => (v.clone(), *e),
                other => return self.fail(&format!("unexpected '{:?}'", other)),
            };
            if !esc && (v == "input" || v == "output" || v == "inout") {
                self.p += 1;
                while self.is_kw("wire") || self.is_kw("reg") || self.is_kw("logic") || self.is_kw("signed") || self.is_kw("tri") {
                    self.p += 1;
                }
                let rng = if self.is_p('[', 0) { Some(self.range()?) } else { None };
                loop {
                    let nm = self.expect_id()?;
                    m.declare(&nm, Some(v.clone()), rng);
                    if self.is_p(',', 0) {
                        self.p += 1;
                        continue;
                    }
                    break;
                }
                self.expect(';')?;
                continue;
            }
            if !esc && ["wire", "reg", "logic", "tri", "wand", "wor", "supply0", "supply1", "tri0", "tri1"].contains(&v.as_str()) {
                self.p += 1;
                while self.is_kw("signed") {
                    self.p += 1;
                }
                let rng = if self.is_p('[', 0) { Some(self.range()?) } else { None };
                loop {
                    let nm = self.expect_id()?;
                    if self.is_p('[', 0) {
                        return self.fail("arrays of nets are not supported");
                    }
                    m.declare(&nm, None, rng);
                    if v == "supply0" || v == "supply1" {
                        m.assigns.push(Assign { lhs: Expr::Id { name: nm.clone(), idx: None, range: None }, rhs: Expr::Const(vec![if v == "supply1" { b'1' } else { b'0' }]), line: tok.line });
                    }
                    if self.is_p('=', 0) {
                        self.p += 1;
                        let rhs = self.expr()?;
                        m.assigns.push(Assign { lhs: Expr::Id { name: nm.clone(), idx: None, range: None }, rhs, line: tok.line });
                    }
                    if self.is_p(',', 0) {
                        self.p += 1;
                        continue;
                    }
                    break;
                }
                self.expect(';')?;
                continue;
            }
            if !esc && v == "assign" {
                self.p += 1;
                loop {
                    let lhs = self.expr()?;
                    self.expect('=')?;
                    let rhs = self.expr()?;
                    m.assigns.push(Assign { lhs, rhs, line: tok.line });
                    if self.is_p(',', 0) {
                        self.p += 1;
                        continue;
                    }
                    break;
                }
                self.expect(';')?;
                continue;
            }
            if !esc && ["parameter", "localparam", "defparam", "genvar", "integer", "real", "time"].contains(&v.as_str()) {
                self.skip_to_semi()?;
                continue;
            }
            if !esc && v == "specify" {
                while self.p < self.t.len() && !self.is_kw("endspecify") {
                    self.p += 1;
                }
                self.p += 1;
                continue;
            }
            if !esc && ["initial", "always", "function", "task", "generate"].contains(&v.as_str()) {
                return self.fail(&format!("'{}' blocks are not part of a gate-level netlist; synthesize the design first", v));
            }
            // instance(s)
            self.p += 1;
            let typ = v.clone();
            let prim = !esc && PRIMS.contains(&typ.as_str());
            if self.is_p('#', 0) {
                self.p += 1;
                if self.is_p('(', 0) {
                    self.skip_balanced('(', ')')?;
                } else {
                    self.p += 1;
                }
            }
            loop {
                let name = if prim && self.is_p('(', 0) { format!("{}_{}", typ, m.insts.len()) } else { self.expect_id()? };
                if self.is_p('[', 0) {
                    return self.fail("arrays of instances are not supported");
                }
                self.expect('(')?;
                let mut inst = Inst { typ: typ.clone(), name, prim, named: None, positional: None, line: tok.line };
                if self.is_p('.', 0) {
                    let mut named = vec![];
                    loop {
                        if self.is_p(')', 0) {
                            break;
                        }
                        self.expect('.')?;
                        if self.is_p('*', 0) {
                            return self.fail(".* connections are not supported");
                        }
                        let pin = self.expect_id()?;
                        self.expect('(')?;
                        let e = if self.is_p(')', 0) { None } else { Some(self.expr()?) };
                        self.expect(')')?;
                        named.push((pin, e));
                        if self.is_p(',', 0) {
                            self.p += 1;
                            continue;
                        }
                        break;
                    }
                    inst.named = Some(named);
                } else {
                    let mut pos = vec![];
                    loop {
                        if self.is_p(')', 0) {
                            break;
                        }
                        if self.is_p(',', 0) {
                            pos.push(None);
                            self.p += 1;
                            continue;
                        }
                        pos.push(Some(self.expr()?));
                        if self.is_p(',', 0) {
                            self.p += 1;
                            if self.is_p(')', 0) {
                                pos.push(None);
                            }
                            continue;
                        }
                        break;
                    }
                    inst.positional = Some(pos);
                }
                self.expect(')')?;
                m.insts.push(inst);
                if self.is_p(',', 0) {
                    self.p += 1;
                    continue;
                }
                break;
            }
            self.expect(';')?;
        }
        if !self.is_kw("endmodule") {
            return self.fail(&format!("module {} has no endmodule", m.name));
        }
        self.p += 1;
        Ok(m)
    }
}

/// modules in file order (name -> module), plus the order list
pub fn parse_verilog(src: &[u8], file: &str) -> Result<(Vec<String>, HashMap<String, Module>), String> {
    let toks = lex(src);
    let mut ps = Ps { t: &toks, p: 0, file };
    let mut order = vec![];
    let mut mods = HashMap::new();
    while ps.p < toks.len() {
        if ps.is_kw("module") || ps.is_kw("macromodule") {
            ps.p += 1;
            let m = ps.module()?;
            if mods.contains_key(&m.name) {
                return ps.fail(&format!("module {} is defined twice", m.name));
            }
            order.push(m.name.clone());
            mods.insert(m.name.clone(), m);
            continue;
        }
        let s = ps.show();
        return ps.fail(&format!("expected 'module' but found '{}'", s));
    }
    Ok((order, mods))
}
