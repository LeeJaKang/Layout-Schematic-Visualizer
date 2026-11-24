//! LEF reader (port of the web viewer's parseLef): layers, vias, sites, macros with pins and obstructions.
use crate::tok::{num, s, Tokens};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Shape {
    pub layer: String,
    /// x1 y1 x2 y2 in microns (normalised)
    pub r: [f64; 4],
}

#[derive(Clone, Debug)]
pub enum MacroShape {
    Rect(Shape),
    Via { name: String, at: [f64; 2] },
}

#[derive(Clone, Debug, Default)]
pub struct Layer {
    pub name: String,
    pub kind: String,
    pub dir: String,
    pub width: Option<f64>,
    pub pitch: Option<f64>,
}

#[derive(Clone, Debug, Default)]
pub struct Via {
    pub name: String,
    pub shapes: Vec<Shape>,
}

#[derive(Clone, Debug, Default)]
pub struct Pin {
    pub name: String,
    pub dir: String,
    pub usage: String,
    pub shapes: Vec<MacroShape>,
}

#[derive(Clone, Debug, Default)]
pub struct Macro {
    pub name: String,
    pub class: String,
    pub origin: [f64; 2],
    pub w: f64,
    pub h: f64,
    pub pins: Vec<Pin>,
    pub obs: Vec<MacroShape>,
}

#[derive(Clone, Debug)]
pub struct Lib {
    pub dbu: f64,
    pub layers: HashMap<String, Layer>,
    pub layer_order: Vec<String>,
    pub vias: HashMap<String, Via>,
    pub sites: HashMap<String, [f64; 2]>,
    pub macros: HashMap<String, Macro>,
}

impl Default for Lib {
    fn default() -> Self {
        Lib { dbu: 1000.0, layers: HashMap::new(), layer_order: vec![], vias: HashMap::new(), sites: HashMap::new(), macros: HashMap::new() }
    }
}

fn rect(a: f64, b: f64, c: f64, d: f64) -> [f64; 4] {
    [a.min(c), b.min(d), a.max(c), b.max(d)]
}

fn n(t: Option<&[u8]>) -> f64 {
    t.and_then(num).unwrap_or(f64::NAN)
}

fn read_shapes(st: &mut Tokens) -> Vec<MacroShape> {
    let mut out = vec![];
    let mut layer = String::new();
    loop {
        let t = match st.next() {
            None => return out,
            Some(t) => t,
        };
        match t {
            b"END" => return out,
            b"LAYER" => {
                layer = s(st.next().unwrap_or(b""));
                st.skip_to(b";");
            }
            b"RECT" => {
                let mut a = st.next();
                if a == Some(b"MASK") {
                    st.next();
                    a = st.next();
                }
                let (x1, y1, x2, y2) = (n(a), n(st.next()), n(st.next()), n(st.next()));
                st.skip_to(b";");
                out.push(MacroShape::Rect(Shape { layer: layer.clone(), r: rect(x1, y1, x2, y2) }));
            }
            b"POLYGON" => {
                let mut pts = vec![];
                loop {
                    match st.next() {
                        None | Some(b";") => break,
                        Some(b"MASK") => {
                            st.next();
                        }
                        Some(a) => pts.push(n(Some(a))),
                    }
                }
                let xs: Vec<f64> = pts.iter().step_by(2).copied().collect();
                let ys: Vec<f64> = pts.iter().skip(1).step_by(2).copied().collect();
                let mn = |v: &Vec<f64>| v.iter().copied().fold(f64::INFINITY, f64::min);
                let mx = |v: &Vec<f64>| v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                out.push(MacroShape::Rect(Shape { layer: layer.clone(), r: [mn(&xs), mn(&ys), mx(&xs), mx(&ys)] }));
            }
            b"VIA" => {
                let mut a = st.next();
                if a == Some(b"MASK") {
                    st.next();
                    a = st.next();
                }
                let x = n(a);
                let y = n(st.next());
                let name = s(st.next().unwrap_or(b""));
                st.skip_to(b";");
                out.push(MacroShape::Via { name, at: [x, y] });
            }
            b";" => {}
            _ => st.skip_to(b";"),
        }
    }
}

fn end_block(st: &mut Tokens, name: &[u8]) -> Result<(), String> {
    loop {
        match st.next() {
            None => return Err(format!("missing END {}", s(name))),
            Some(b"END") => {
                if st.next() == Some(name) {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
}

pub fn parse_lef(text: &[u8], file: &str, lib: &mut Lib) -> Result<(), String> {
    let mut st = Tokens::new(text);
    let err = |st: &Tokens, m: &str| format!("{}: line {}: {}", file, st.line, m);
    while let Some(t) = st.next() {
        // top-level keywords in any case: IHP's sg13g2_tech.lef writes "Via Via1_XX DEFAULT"
        let up = t.to_ascii_uppercase();
        match up.as_slice() {
            b"UNITS" => loop {
                match st.next() {
                    None => break,
                    Some(b"END") => {
                        st.next();
                        break;
                    }
                    Some(b"DATABASE") => {
                        st.next();
                        lib.dbu = n(st.next());
                        st.skip_to(b";");
                    }
                    Some(b";") => {}
                    Some(_) => st.skip_to(b";"),
                }
            },
            b"LAYER" => {
                let name = s(st.next().unwrap_or(b""));
                if !lib.layers.contains_key(&name) {
                    lib.layers.insert(name.clone(), Layer { name: name.clone(), ..Default::default() });
                    lib.layer_order.push(name.clone());
                }
                loop {
                    let k = st.next().ok_or_else(|| err(&st, &format!("missing END {}", name)))?;
                    match k {
                        b"END" => {
                            if st.peek().map(|x| x == name.as_bytes()).unwrap_or(false) {
                                st.next();
                                break;
                            }
                        }
                        b"TYPE" => {
                            let v = s(st.next().unwrap_or(b""));
                            lib.layers.get_mut(&name).unwrap().kind = v;
                            st.skip_to(b";");
                        }
                        b"DIRECTION" => {
                            let v = s(st.next().unwrap_or(b""));
                            lib.layers.get_mut(&name).unwrap().dir = v;
                            st.skip_to(b";");
                        }
                        b"WIDTH" => {
                            let v = n(st.next());
                            lib.layers.get_mut(&name).unwrap().width = Some(v);
                            st.skip_to(b";");
                        }
                        b"PITCH" => {
                            let v = n(st.next());
                            lib.layers.get_mut(&name).unwrap().pitch = Some(v);
                            st.skip_to(b";");
                        }
                        b";" => {}
                        _ => st.skip_to(b";"),
                    }
                }
            }
            b"VIA" => {
                let name = s(st.next().unwrap_or(b""));
                while matches!(st.peek(), Some(b"DEFAULT") | Some(b"GENERATED") | Some(b"Default")) {
                    st.next();
                }
                let mut via = Via { name: name.clone(), shapes: vec![] };
                let mut layer = String::new();
                loop {
                    let k = st.next().ok_or_else(|| err(&st, &format!("missing END {}", name)))?;
                    match k {
                        b"END" => {
                            if st.next().map(|x| x == name.as_bytes()).unwrap_or(false) {
                                break;
                            }
                        }
                        b"LAYER" => {
                            layer = s(st.next().unwrap_or(b""));
                            st.skip_to(b";");
                        }
                        b"RECT" => {
                            let mut a = st.next();
                            if a == Some(b"MASK") {
                                st.next();
                                a = st.next();
                            }
                            let (x1, y1, x2, y2) = (n(a), n(st.next()), n(st.next()), n(st.next()));
                            st.skip_to(b";");
                            via.shapes.push(Shape { layer: layer.clone(), r: rect(x1, y1, x2, y2) });
                        }
                        b";" => {}
                        _ => st.skip_to(b";"),
                    }
                }
                lib.vias.insert(name, via);
            }
            b"SITE" => {
                let name = s(st.next().unwrap_or(b""));
                let mut sz = [0.0, 0.0];
                loop {
                    let k = st.next().ok_or_else(|| err(&st, &format!("missing END {}", name)))?;
                    match k {
                        b"END" => {
                            if st.next().map(|x| x == name.as_bytes()).unwrap_or(false) {
                                break;
                            }
                        }
                        b"SIZE" => {
                            sz[0] = n(st.next());
                            st.next();
                            sz[1] = n(st.next());
                            st.skip_to(b";");
                        }
                        b";" => {}
                        _ => st.skip_to(b";"),
                    }
                }
                lib.sites.insert(name, sz);
            }
            b"MACRO" => {
                let name = s(st.next().unwrap_or(b""));
                let mut m = Macro { name: name.clone(), ..Default::default() };
                loop {
                    let k = st.next().ok_or_else(|| err(&st, &format!("missing END {}", name)))?;
                    match k {
                        b"END" => {
                            if st.next().map(|x| x == name.as_bytes()).unwrap_or(false) {
                                break;
                            }
                        }
                        b"CLASS" => {
                            let mut parts = vec![];
                            loop {
                                match st.next() {
                                    None | Some(b";") => break,
                                    Some(a) => parts.push(s(a)),
                                }
                            }
                            m.class = parts.join(" ");
                        }
                        b"ORIGIN" => {
                            m.origin = [n(st.next()), n(st.next())];
                            st.skip_to(b";");
                        }
                        b"SIZE" => {
                            m.w = n(st.next());
                            st.next();
                            m.h = n(st.next());
                            st.skip_to(b";");
                        }
                        b"PIN" => {
                            let pn = s(st.next().unwrap_or(b""));
                            let mut pin = Pin { name: pn.clone(), ..Default::default() };
                            loop {
                                let a = st.next().ok_or_else(|| err(&st, &format!("missing END {}", pn)))?;
                                match a {
                                    b"END" => {
                                        if st.next().map(|x| x == pn.as_bytes()).unwrap_or(false) {
                                            break;
                                        }
                                    }
                                    b"DIRECTION" => {
                                        pin.dir = s(st.next().unwrap_or(b""));
                                        st.skip_to(b";");
                                    }
                                    b"USE" => {
                                        pin.usage = s(st.next().unwrap_or(b""));
                                        st.skip_to(b";");
                                    }
                                    b"PORT" => pin.shapes.extend(read_shapes(&mut st)),
                                    b";" => {}
                                    _ => st.skip_to(b";"),
                                }
                            }
                            m.pins.push(pin);
                        }
                        b"OBS" => m.obs.extend(read_shapes(&mut st)),
                        b";" => {}
                        _ => st.skip_to(b";"),
                    }
                }
                lib.macros.insert(name, m);
            }
            b"VIARULE" | b"NONDEFAULTRULE" => {
                let name = st.next().unwrap_or(b"");
                end_block(&mut st, name).map_err(|e| err(&st, &e))?;
            }
            b"PROPERTYDEFINITIONS" | b"SPACING" | b"NOISETABLE" | b"CORRECTIONTABLE" => {
                end_block(&mut st, t).map_err(|e| err(&st, &e))?;
            }
            b"BEGINEXT" => end_block(&mut st, b"ENDEXT").map_err(|e| err(&st, &e))?,
            b"END" => {
                st.next();
            }
            _ => st.skip_to(b";"),
        }
    }
    Ok(())
}
