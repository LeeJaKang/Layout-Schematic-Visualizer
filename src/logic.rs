//! Boolean functions from Liberty, truth tables, and recovery of a gate tree with minimal bubbles
//! (port of the web viewer's logic.js; results are compared with it gate for gate).

#[derive(Debug, Clone)]
pub enum Expr {
    Var(String),
    Const(u8),
    Not(Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Xor(Vec<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Op(char),
    Id(String),
    C(u8),
}

fn tokenize(s: &str) -> Result<Vec<Tok>, String> {
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut out = vec![];
    while i < b.len() {
        let c = b[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if "!'^&*|+()".contains(c) {
            out.push(Tok::Op(c));
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == '_' {
            let st = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], '_' | '[' | ']' | '.' | '$')) {
                i += 1;
            }
            out.push(Tok::Id(b[st..i].iter().collect()));
            continue;
        }
        if c == '0' || c == '1' {
            // a constant only when not followed by another word character
            let next = b.get(i + 1).copied();
            if next.map(|n| !(n.is_ascii_alphanumeric() || n == '_')).unwrap_or(true) {
                out.push(Tok::C(if c == '1' { 1 } else { 0 }));
                i += 1;
                continue;
            }
        }
        return Err(format!("cannot read '{}' in function \"{}\"", c, s));
    }
    Ok(out)
}

pub fn parse_bool(s: &str) -> Result<Expr, String> {
    let toks = tokenize(s)?;
    let mut p = 0usize;
    fn starts(t: Option<&Tok>) -> bool {
        matches!(t, Some(Tok::Op('!')) | Some(Tok::Id(_)) | Some(Tok::C(_)) | Some(Tok::Op('(')))
    }
    fn or(t: &[Tok], p: &mut usize, s: &str) -> Result<Expr, String> {
        let mut a = vec![and(t, p, s)?];
        while matches!(t.get(*p), Some(Tok::Op('|')) | Some(Tok::Op('+'))) {
            *p += 1;
            a.push(and(t, p, s)?);
        }
        Ok(if a.len() == 1 { a.pop().unwrap() } else { Expr::Or(a) })
    }
    fn and(t: &[Tok], p: &mut usize, s: &str) -> Result<Expr, String> {
        let mut a = vec![xor(t, p, s)?];
        loop {
            if matches!(t.get(*p), Some(Tok::Op('&')) | Some(Tok::Op('*'))) {
                *p += 1;
                a.push(xor(t, p, s)?);
                continue;
            }
            if starts(t.get(*p)) {
                a.push(xor(t, p, s)?);
                continue;
            }
            break;
        }
        Ok(if a.len() == 1 { a.pop().unwrap() } else { Expr::And(a) })
    }
    fn xor(t: &[Tok], p: &mut usize, s: &str) -> Result<Expr, String> {
        let mut a = vec![unary(t, p, s)?];
        while t.get(*p) == Some(&Tok::Op('^')) {
            *p += 1;
            a.push(unary(t, p, s)?);
        }
        Ok(if a.len() == 1 { a.pop().unwrap() } else { Expr::Xor(a) })
    }
    fn unary(t: &[Tok], p: &mut usize, s: &str) -> Result<Expr, String> {
        if t.get(*p) == Some(&Tok::Op('!')) {
            *p += 1;
            return Ok(Expr::Not(Box::new(unary(t, p, s)?)));
        }
        let mut e = primary(t, p, s)?;
        while t.get(*p) == Some(&Tok::Op('\'')) {
            *p += 1;
            e = Expr::Not(Box::new(e));
        }
        Ok(e)
    }
    fn primary(t: &[Tok], p: &mut usize, s: &str) -> Result<Expr, String> {
        match t.get(*p) {
            None => Err(format!("function \"{}\" ends early", s)),
            Some(Tok::Id(v)) => {
                *p += 1;
                Ok(Expr::Var(v.clone()))
            }
            Some(Tok::C(v)) => {
                *p += 1;
                Ok(Expr::Const(*v))
            }
            Some(Tok::Op('(')) => {
                *p += 1;
                let e = or(t, p, s)?;
                if t.get(*p) != Some(&Tok::Op(')')) {
                    return Err(format!("missing ) in function \"{}\"", s));
                }
                *p += 1;
                Ok(e)
            }
            Some(x) => Err(format!("unexpected '{:?}' in function \"{}\"", x, s)),
        }
    }
    let e = or(&toks, &mut p, s)?;
    if p != toks.len() {
        return Err(format!("extra text in function \"{}\"", s));
    }
    Ok(e)
}

pub fn expr_vars(e: &Expr, acc: &mut Vec<String>) {
    match e {
        Expr::Var(n) => {
            if !acc.contains(n) {
                acc.push(n.clone())
            }
        }
        Expr::Const(_) => {}
        Expr::Not(a) => expr_vars(a, acc),
        Expr::And(v) | Expr::Or(v) | Expr::Xor(v) => v.iter().for_each(|x| expr_vars(x, acc)),
    }
}

fn eval_expr(e: &Expr, vars: &[String], idx: usize) -> u8 {
    match e {
        Expr::Var(n) => ((idx >> vars.iter().position(|v| v == n).unwrap()) & 1) as u8,
        Expr::Const(v) => *v,
        Expr::Not(a) => eval_expr(a, vars, idx) ^ 1,
        Expr::And(v) => v.iter().all(|x| eval_expr(x, vars, idx) == 1) as u8,
        Expr::Or(v) => v.iter().any(|x| eval_expr(x, vars, idx) == 1) as u8,
        Expr::Xor(v) => v.iter().fold(0, |a, x| a ^ eval_expr(x, vars, idx)),
    }
}

const MAX_VARS: usize = 12;

#[derive(Clone, Debug)]
pub struct Func {
    pub vars: Vec<String>,
    pub tt: Vec<u8>,
}

fn depends_on(tt: &[u8], k: usize) -> bool {
    let bit = 1 << k;
    (0..tt.len()).any(|i| i & bit == 0 && tt[i] != tt[i | bit])
}
fn project(f: &Func, keep: usize) -> Func {
    let pos: Vec<usize> = (0..f.vars.len()).filter(|k| keep & (1 << k) != 0).collect();
    let mut tt = vec![0u8; 1 << pos.len()];
    for (j, t) in tt.iter_mut().enumerate() {
        let mut idx = 0;
        for (b, &p) in pos.iter().enumerate() {
            if j & (1 << b) != 0 {
                idx |= 1 << p;
            }
        }
        *t = f.tt[idx];
    }
    Func { vars: pos.iter().map(|&k| f.vars[k].clone()).collect(), tt }
}
fn reduce_support(f: &Func) -> Func {
    let mut mask = 0usize;
    for k in 0..f.vars.len() {
        if depends_on(&f.tt, k) {
            mask |= 1 << k;
        }
    }
    if mask == (1 << f.vars.len()) - 1 {
        return f.clone();
    }
    project(f, mask)
}
fn cofactor(f: &Func, k: usize, val: usize) -> Func {
    let n = f.vars.len();
    let vars: Vec<String> = f.vars.iter().enumerate().filter(|(i, _)| *i != k).map(|(_, v)| v.clone()).collect();
    let mut tt = vec![0u8; 1 << (n - 1)];
    for (j, t) in tt.iter_mut().enumerate() {
        let lo = j & ((1 << k) - 1);
        let hi = (j >> k) << (k + 1);
        *t = f.tt[hi | lo | (val << k)];
    }
    Func { vars, tt }
}
fn subsets_by_size(m: usize) -> Vec<usize> {
    let full = (1usize << m) - 1;
    let mut v: Vec<usize> = (1..full).collect();
    v.sort_by(|a, b| a.count_ones().cmp(&b.count_ones()).then(a.cmp(b)));
    v
}
fn try_and(tt: &[u8], m: usize, s: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let t = ((1usize << m) - 1) & !s;
    let mut g1 = vec![0u8; 1 << m];
    let mut g2 = vec![0u8; 1 << m];
    for i in 0..tt.len() {
        if tt[i] == 1 {
            g1[i & s] = 1;
            g2[i & t] = 1;
        }
    }
    for i in 0..tt.len() {
        if tt[i] != (g1[i & s] & g2[i & t]) {
            return None;
        }
    }
    Some((g1, g2))
}
fn try_xor(tt: &[u8], m: usize, s: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let t = ((1usize << m) - 1) & !s;
    let f0 = tt[0];
    for i in 0..tt.len() {
        if tt[i] != (tt[i & s] ^ tt[i & t] ^ f0) {
            return None;
        }
    }
    let mut g1 = vec![0u8; 1 << m];
    let mut g2 = vec![0u8; 1 << m];
    for i in 0..tt.len() {
        g1[i & s] = tt[i & s];
        g2[i & t] = tt[i & t] ^ f0;
    }
    Some((g1, g2))
}
fn sub_fn(vars: &[String], arr: &[u8], mask: usize) -> Func {
    let pos: Vec<usize> = (0..vars.len()).filter(|k| mask & (1 << k) != 0).collect();
    let mut tt = vec![0u8; 1 << pos.len()];
    for (j, t) in tt.iter_mut().enumerate() {
        let mut idx = 0;
        for (b, &p) in pos.iter().enumerate() {
            if j & (1 << b) != 0 {
                idx |= 1 << p;
            }
        }
        *t = arr[idx];
    }
    Func { vars: pos.iter().map(|&k| vars[k].clone()).collect(), tt }
}
fn is_const(tt: &[u8]) -> bool {
    tt.iter().all(|&x| x == tt[0])
}
fn maj(a: u8, b: u8, c: u8) -> u8 {
    (a & b) | (a & c) | (b & c)
}

#[derive(Debug, Clone)]
pub enum Node {
    Const(u8),
    Lit(String, bool),
    Not(Box<Node>),
    And(Vec<Node>),
    Or(Vec<Node>),
    Xor(Vec<Node>),
    Mux(String, bool, Box<Node>, Box<Node>),
    Maj(Vec<Node>),
    Box(Vec<String>, Vec<u8>),
}

fn negate(x: Node) -> Node {
    match x {
        Node::Lit(v, n) => Node::Lit(v, !n),
        Node::Const(v) => Node::Const(v ^ 1),
        o => Node::Not(Box::new(o)),
    }
}

pub fn decompose(f0: &Func) -> Node {
    let f = reduce_support(f0);
    let m = f.vars.len();
    if m == 0 {
        return Node::Const(f.tt[0]);
    }
    if m == 1 {
        return Node::Lit(f.vars[0].clone(), f.tt[0] == 1);
    }
    let subsets = subsets_by_size(m);
    let full = (1usize << m) - 1;
    for kind in 0..2 {
        let tt: Vec<u8> = if kind == 0 { f.tt.clone() } else { f.tt.iter().map(|x| x ^ 1).collect() };
        for &s in &subsets {
            if s & 1 == 0 {
                continue;
            }
            if let Some((r0, r1)) = try_and(&tt, m, s) {
                let mut a = sub_fn(&f.vars, &r0, s);
                let mut b = sub_fn(&f.vars, &r1, full & !s);
                if kind == 1 {
                    a.tt.iter_mut().for_each(|x| *x ^= 1);
                    b.tt.iter_mut().for_each(|x| *x ^= 1);
                }
                let (na, nb) = (decompose(&a), decompose(&b));
                let mut kids = vec![];
                for x in [na, nb] {
                    match (kind, x) {
                        (0, Node::And(c)) => kids.extend(c),
                        (1, Node::Or(c)) => kids.extend(c),
                        (_, x) => kids.push(x),
                    }
                }
                return if kind == 0 { Node::And(kids) } else { Node::Or(kids) };
            }
        }
    }
    for &s in &subsets {
        if s & 1 == 0 {
            continue;
        }
        if let Some((r0, r1)) = try_xor(&f.tt, m, s) {
            let na = decompose(&sub_fn(&f.vars, &r0, s));
            let nb = decompose(&sub_fn(&f.vars, &r1, full & !s));
            let mut kids = vec![];
            let mut neg = false;
            for x in [na, nb] {
                match x {
                    Node::Xor(c) => kids.extend(c),
                    Node::Const(v) => neg ^= v == 1,
                    x => kids.push(x),
                }
            }
            if neg && !kids.is_empty() {
                let k0 = kids.remove(0);
                kids.insert(0, negate(k0));
            }
            return Node::Xor(kids);
        }
    }
    if m == 3 {
        for mask in 0..8usize {
            for o in 0..2u8 {
                let ok = (0..8usize).all(|i| {
                    let x = i ^ mask;
                    f.tt[i] == (maj((x & 1) as u8, ((x >> 1) & 1) as u8, ((x >> 2) & 1) as u8) ^ o)
                });
                if ok {
                    let mm = if o == 1 { mask ^ 7 } else { mask };
                    return Node::Maj(f.vars.iter().enumerate().map(|(j, v)| Node::Lit(v.clone(), mm & (1 << j) != 0)).collect());
                }
            }
        }
    }
    let mut best: Option<(usize, Func, Func, usize, usize)> = None; // k f0 f1 score shared
    for k in 0..m {
        let a = reduce_support(&cofactor(&f, k, 0));
        let b = reduce_support(&cofactor(&f, k, 1));
        if is_const(&a.tt) || is_const(&b.tt) {
            continue;
        }
        let shared = a.vars.iter().filter(|v| b.vars.contains(v)).count();
        let score = shared * 100 + a.vars.len() + b.vars.len();
        if best.as_ref().map(|x| score < x.3).unwrap_or(true) {
            best = Some((k, a, b, score, shared));
        }
    }
    if let Some((k, a, b, _, shared)) = best {
        if shared == 0 || m <= 6 {
            return Node::Mux(f.vars[k].clone(), false, Box::new(decompose(&a)), Box::new(decompose(&b)));
        }
    }
    Node::Box(f.vars.clone(), f.tt.clone())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GT {
    And,
    Or,
    Xor,
    Buf,
    Mux,
    Maj,
    Box,
    Const,
}

#[derive(Debug, Clone)]
pub enum Src {
    Pin(String, bool),
    Gate(Box<Gate>),
}

#[derive(Debug, Clone)]
pub struct Gate {
    pub gate: GT,
    pub out: bool,
    pub ins: Vec<Src>,
    pub sel: Option<Box<Src>>,
    pub v: u8,
    pub vars: Vec<String>,
    pub tt: Vec<u8>,
}

fn g(gate: GT, out: bool, ins: Vec<Src>) -> Gate {
    Gate { gate, out, ins, sel: None, v: 0, vars: vec![], tt: vec![] }
}

fn realize(t: &Node, p: u8) -> (u32, Src) {
    match t {
        Node::Const(v) => (0, Src::Gate(Box::new(Gate { v: v ^ p, ..g(GT::Const, false, vec![]) }))),
        Node::Lit(v, neg) => {
            let b = (*neg as u8) ^ p;
            (b as u32, Src::Pin(v.clone(), b == 1))
        }
        Node::Not(a) => realize(a, p ^ 1),
        Node::And(c) | Node::Or(c) => {
            let is_and = matches!(t, Node::And(_));
            let (nat, dual) = if is_and { (GT::And, GT::Or) } else { (GT::Or, GT::And) };
            let kn: Vec<(u32, Src)> = c.iter().map(|x| realize(x, 0)).collect();
            let kd: Vec<(u32, Src)> = c.iter().map(|x| realize(x, 1)).collect();
            let cn = p as u32 + kn.iter().map(|r| r.0).sum::<u32>();
            let cd = (1 - p) as u32 + kd.iter().map(|r| r.0).sum::<u32>();
            if cn <= cd {
                (cn, Src::Gate(Box::new(g(nat, p == 1, kn.into_iter().map(|r| r.1).collect()))))
            } else {
                (cd, Src::Gate(Box::new(g(dual, p == 0, kd.into_iter().map(|r| r.1).collect()))))
            }
        }
        Node::Xor(c) => {
            let mut parity = p;
            let mut cost = 0;
            let mut ins = vec![];
            for x in c {
                let r0 = realize(x, 0);
                let r1 = realize(x, 1);
                if r1.0 < r0.0 {
                    parity ^= 1;
                    cost += r1.0;
                    ins.push(r1.1);
                } else {
                    cost += r0.0;
                    ins.push(r0.1);
                }
            }
            (cost + parity as u32, Src::Gate(Box::new(g(GT::Xor, parity == 1, ins))))
        }
        Node::Mux(s, sneg, d0, d1) => {
            let (d0, d1) = if *sneg { (d1, d0) } else { (d0, d1) };
            let sel = Some(Box::new(Src::Pin(s.clone(), false)));
            let (a0, a1) = (realize(d0, p), realize(d1, p));
            let (b0, b1) = (realize(d0, p ^ 1), realize(d1, p ^ 1));
            let (ca, cb) = (a0.0 + a1.0, 1 + b0.0 + b1.0);
            if ca <= cb {
                (ca, Src::Gate(Box::new(Gate { sel, ..g(GT::Mux, false, vec![a0.1, a1.1]) })))
            } else {
                (cb, Src::Gate(Box::new(Gate { sel, ..g(GT::Mux, true, vec![b0.1, b1.1]) })))
            }
        }
        Node::Maj(c) => {
            let mut best: Option<(u32, Src)> = None;
            for q in 0..2u8 {
                let kids: Vec<(u32, Src)> = c.iter().map(|x| realize(x, q)).collect();
                let cost = (q ^ p) as u32 + kids.iter().map(|r| r.0).sum::<u32>();
                if best.as_ref().map(|b| cost < b.0).unwrap_or(true) {
                    best = Some((cost, Src::Gate(Box::new(g(GT::Maj, (q ^ p) == 1, kids.into_iter().map(|r| r.1).collect())))));
                }
            }
            best.unwrap()
        }
        Node::Box(vars, tt) => (0, Src::Gate(Box::new(Gate { vars: vars.clone(), tt: tt.clone(), ..g(GT::Box, p == 1, vars.iter().map(|v| Src::Pin(v.clone(), false)).collect()) }))),
    }
}

pub fn count_gates(s: &Src) -> usize {
    match s {
        Src::Pin(..) => 0,
        Src::Gate(g) => 1 + g.ins.iter().map(count_gates).sum::<usize>() + g.sel.as_ref().map(|x| count_gates(x)).unwrap_or(0),
    }
}

fn eval_drawn(s: &Src, vars: &[String], idx: usize) -> u8 {
    let val = |n: &str| ((idx >> vars.iter().position(|v| v == n).unwrap()) & 1) as u8;
    match s {
        Src::Pin(p, b) => val(p) ^ (*b as u8),
        Src::Gate(gt) => {
            let v: Vec<u8> = gt.ins.iter().map(|x| eval_drawn(x, vars, idx)).collect();
            let r = match gt.gate {
                GT::Const => gt.v,
                GT::Buf => v[0],
                GT::And => v.iter().fold(1, |a, b| a & b),
                GT::Or => v.iter().fold(0, |a, b| a | b),
                GT::Xor => v.iter().fold(0, |a, b| a ^ b),
                GT::Maj => maj(v[0], v[1], v[2]),
                GT::Mux => {
                    if eval_drawn(gt.sel.as_ref().unwrap(), vars, idx) == 1 { v[1] } else { v[0] }
                }
                GT::Box => {
                    let mut i = 0;
                    for (k, nm) in gt.vars.iter().enumerate() {
                        if val(nm) == 1 {
                            i |= 1 << k;
                        }
                    }
                    gt.tt[i]
                }
            };
            r ^ (gt.out as u8)
        }
    }
}

const MAX_GATES: usize = 10;

/// drawn gate for one output function; errors if the drawing is not equivalent
pub fn synthesize_output(func: &str, pin_order: &[String]) -> Result<Gate, String> {
    let ast = parse_bool(func)?;
    let mut used = vec![];
    expr_vars(&ast, &mut used);
    let mut vars: Vec<String> = pin_order.iter().filter(|p| used.contains(p)).cloned().collect();
    for u in &used {
        if !pin_order.contains(u) {
            vars.push(u.clone());
        }
    }
    if vars.len() > MAX_VARS {
        return Ok(Gate { vars: vars.clone(), ..g(GT::Box, false, vars.iter().map(|v| Src::Pin(v.clone(), false)).collect()) });
    }
    let tt: Vec<u8> = (0..1usize << vars.len()).map(|i| eval_expr(&ast, &vars, i)).collect();
    let f = Func { vars: vars.clone(), tt: tt.clone() };
    let tree = decompose(&f);
    let (_, mut s) = realize(&tree, 0);
    if let Src::Pin(p, b) = &s {
        s = Src::Gate(Box::new(g(GT::Buf, *b, vec![Src::Pin(p.clone(), false)])));
    }
    if count_gates(&s) > MAX_GATES {
        let sv = reduce_support(&f).vars;
        s = Src::Gate(Box::new(Gate { vars: f.vars.clone(), tt: f.tt.clone(), ..g(GT::Box, false, sv.iter().map(|v| Src::Pin(v.clone(), false)).collect()) }));
    }
    for (i, &want) in tt.iter().enumerate() {
        if eval_drawn(&s, &vars, i) != want {
            return Err(format!("internal: drawing of \"{}\" is not equivalent at input {}", func, i));
        }
    }
    match s {
        Src::Gate(gt) => Ok(*gt),
        Src::Pin(..) => unreachable!(),
    }
}

/// short description, identical to the web viewer's describeGate
pub fn describe(s: &Src) -> String {
    match s {
        Src::Pin(p, b) => format!("{}{}", if *b { "!" } else { "" }, p),
        Src::Gate(gt) => describe_gate(gt),
    }
}
pub fn describe_gate(gt: &Gate) -> String {
    let nm = match gt.gate {
        GT::And => if gt.out { "NAND" } else { "AND" },
        GT::Or => if gt.out { "NOR" } else { "OR" },
        GT::Xor => if gt.out { "XNOR" } else { "XOR" },
        GT::Buf => if gt.out { "INV" } else { "BUF" },
        GT::Mux => if gt.out { "MUXI" } else { "MUX" },
        GT::Maj => if gt.out { "MAJI" } else { "MAJ" },
        GT::Box => "FUNC",
        GT::Const => return format!("TIE{}", gt.v),
    };
    let mut inner: Vec<String> = gt.ins.iter().map(describe).collect();
    if let Some(sel) = &gt.sel {
        inner.insert(0, format!("S={}", describe(sel)));
    }
    format!("{}({})", nm, inner.join(", "))
}
