//! Liberty cell -> drawing model (port of the web viewer's cells.js).
use crate::liberty::LCell;
use crate::logic::{expr_vars, parse_bool, synthesize_output, Gate, Src, GT};
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Gate,
    Compound,
    Seq,
    Tri,
    Tie,
    Box,
}

#[derive(Debug, Clone)]
pub struct Model {
    pub name: String,
    pub title: String,
    pub kind: Kind,
    pub kind_text: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub inouts: Vec<String>,
    pub pin_dir: HashMap<String, String>,
    pub pg_pins: Vec<String>,
    pub funcs: BTreeMap<String, String>,
    pub out_gates: BTreeMap<String, Gate>,
    pub clocks: Vec<String>,
    pub unknown: bool,
    pub tie_value: u8,
    pub enable: Option<String>,
}

fn vars_of(text: &Option<String>) -> Vec<String> {
    match text {
        None => vec![],
        Some(t) => parse_bool(t).map(|e| {
            let mut v = vec![];
            expr_vars(&e, &mut v);
            v
        })
        .unwrap_or_default(),
    }
}

pub fn short_title(name: &str) -> String {
    let mut s = name.to_string();
    if let Some(i) = s.find("_ASAP7_") {
        if s[i + 7..].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            s.truncate(i);
        }
    }
    for p in ["sky130_fd_sc_", "gf180mcu_fd_sc_"] {
        if let Some(rest) = s.strip_prefix(p) {
            if let Some(k) = rest.find("__") {
                s = rest[k + 2..].to_string();
            }
        }
    }
    s
}

fn single_gate(g: &Gate) -> bool {
    g.ins.iter().all(|s| matches!(s, Src::Pin(..))) && g.sel.as_ref().map(|s| matches!(**s, Src::Pin(..))).unwrap_or(true)
}

pub fn analyze_cell(c: &LCell) -> Model {
    let by = |d: &str| c.pins.iter().filter(|p| p.dir == d).map(|p| p.name.clone()).collect::<Vec<_>>();
    let (inputs, outputs, inouts) = (by("input"), by("output"), by("inout"));
    let mut pin_dir = HashMap::new();
    for p in &c.pins {
        pin_dir.insert(p.name.clone(), if p.dir.is_empty() { "input".to_string() } else { p.dir.clone() });
    }
    let mut funcs = BTreeMap::new();
    for p in &c.pins {
        if let Some(f) = &p.func {
            funcs.insert(p.name.clone(), f.clone());
        }
    }
    let mut m = Model {
        name: c.name.clone(),
        title: short_title(&c.name),
        kind: Kind::Box,
        kind_text: String::new(),
        inputs: inputs.clone(),
        outputs: outputs.clone(),
        inouts: inouts.clone(),
        pin_dir,
        pg_pins: c.pg_pins.clone(),
        funcs,
        out_gates: BTreeMap::new(),
        clocks: vec![],
        unknown: false,
        tie_value: 0,
        enable: None,
    };
    let st = c.ff.as_ref().or(c.latch.as_ref());
    if st.is_some() || c.icg || c.statetable || c.bank {
        let mut clocks: Vec<String> = vec![];
        let mut add = |v: Vec<String>| {
            for x in v {
                if !clocks.contains(&x) {
                    clocks.push(x);
                }
            }
        };
        if let Some(s) = st {
            add(vars_of(&s.clocked_on));
            add(vars_of(&s.clocked_on_also));
            add(vars_of(&s.enable));
        }
        add(c.pins.iter().filter(|p| p.gate_clock || p.clock).map(|p| p.name.clone()).collect());
        m.kind = Kind::Seq;
        m.kind_text = if c.ff.is_some() { "DFF" } else if c.latch.is_some() { "LATCH" } else if c.icg { "ICG" } else if c.bank { "BANK" } else { "STATE" }.into();
        m.clocks = clocks.into_iter().filter(|x| inputs.contains(x)).collect();
        return m;
    }
    let mut all_ok = !outputs.is_empty();
    let order: Vec<String> = inputs.iter().chain(inouts.iter()).cloned().collect();
    for o in &outputs {
        let pin = c.pins.iter().find(|p| &p.name == o).unwrap();
        match &pin.func {
            None => all_ok = false,
            Some(f) => match synthesize_output(f, &order) {
                Ok(g) => {
                    m.out_gates.insert(o.clone(), g);
                }
                Err(_) => all_ok = false,
            },
        }
    }
    if !all_ok || !inouts.is_empty() {
        m.kind = Kind::Box;
        return m;
    }
    if let Some(tri) = c.pins.iter().find(|p| p.dir == "output" && p.three_state.is_some()) {
        if outputs.len() == 1 {
            let g = &m.out_gates[&tri.name];
            let ev = vars_of(&tri.three_state);
            if g.gate == GT::Buf && ev.len() == 1 {
                m.kind = Kind::Tri;
                m.enable = Some(ev[0].clone());
                return m;
            }
            m.kind = Kind::Box;
            return m;
        }
    }
    if outputs.len() == 1 {
        let g = &m.out_gates[&outputs[0]];
        if g.gate == GT::Const {
            m.kind = Kind::Tie;
            m.tie_value = g.v;
            return m;
        }
        if single_gate(g) {
            m.kind = Kind::Gate;
            return m;
        }
    }
    m.kind = Kind::Compound;
    m
}

/// a cell used by the netlist but missing from the loaded libraries
pub fn unknown_cell(name: &str, pins: &[String], guess: impl Fn(&str) -> &'static str) -> Model {
    let mut m = Model {
        name: name.into(),
        title: short_title(name),
        kind: Kind::Box,
        kind_text: String::new(),
        inputs: vec![],
        outputs: vec![],
        inouts: vec![],
        pin_dir: HashMap::new(),
        pg_pins: vec![],
        funcs: BTreeMap::new(),
        out_gates: BTreeMap::new(),
        clocks: vec![],
        unknown: true,
        tie_value: 0,
        enable: None,
    };
    for p in pins {
        let d = guess(p);
        m.pin_dir.insert(p.clone(), d.into());
        match d {
            "output" => m.outputs.push(p.clone()),
            "inout" => m.inouts.push(p.clone()),
            _ => m.inputs.push(p.clone()),
        }
    }
    m
}

impl Model {
    pub fn kind_name(&self) -> &'static str {
        match self.kind {
            Kind::Gate => "gate",
            Kind::Compound => "compound",
            Kind::Seq => "seq",
            Kind::Tri => "tri",
            Kind::Tie => "tie",
            Kind::Box => "box",
        }
    }
}
