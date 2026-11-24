//! GDSII stream reader (port of the web viewer's gds.js): structures, boundaries/boxes, paths, texts,
//! (arrayed) references, and flattening of one cell.
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Poly {
    pub layer: i16,
    pub dt: i16,
    pub pts: Vec<i32>, // x0 y0 x1 y1 ... (closing point removed)
}

#[derive(Clone, Debug)]
pub struct Text {
    pub layer: i16,
    pub dt: i16,
    pub string: String,
    pub x: i32,
    pub y: i32,
    /// hierarchy depth after flattening (0 = written in the flattened cell itself)
    pub depth: u16,
}

#[derive(Clone, Debug, Default)]
pub struct PathEl {
    pub layer: i16,
    pub dt: i16,
    pub width: i32,
    pub pathtype: i16,
    pub bgn: i32,
    pub end: i32,
    pub xy: Vec<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct Ref {
    pub sname: String,
    pub aref: bool,
    pub mag: f64,
    pub angle: f64,
    pub reflect: bool,
    pub cols: i32,
    pub rows: i32,
    pub xy: Vec<i32>,
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    pub name: String,
    pub polys: Vec<Poly>,
    pub paths: Vec<PathEl>,
    pub texts: Vec<Text>,
    pub refs: Vec<Ref>,
}

#[derive(Clone, Debug, Default)]
pub struct GdsLib {
    pub name: String,
    pub user_per_db: f64,
    pub meters_per_db: f64,
    pub cells: HashMap<String, Cell>,
    pub order: Vec<String>,
}

/// JavaScript Math.round
fn jr(v: f64) -> i32 {
    (v + 0.5).floor() as i32
}

fn r8(b: &[u8]) -> f64 {
    let sign = if b[0] & 0x80 != 0 { -1.0 } else { 1.0 };
    let exp = (b[0] & 0x7f) as i32 - 64;
    let mut m: f64 = 0.0;
    for &x in &b[1..8] {
        m = m * 256.0 + x as f64;
    }
    sign * m * 16f64.powi(exp) / 2f64.powi(56)
}

enum El {
    None,
    Poly(i16, i16, Vec<i32>),
    Path(PathEl),
    Ref(Ref),
    Text(i16, i16, String, Vec<i32>),
}

pub fn parse_gds(b: &[u8], file: &str) -> Result<GdsLib, String> {
    let mut lib = GdsLib { user_per_db: 0.001, meters_per_db: 1e-9, ..Default::default() };
    let mut cell: Option<Cell> = None;
    let mut el = El::None;
    let n = b.len();
    let mut o = 0usize;
    let s = |p: &[u8]| String::from_utf8_lossy(p).trim_end_matches('\0').to_string();
    while o + 4 <= n {
        let len = u16::from_be_bytes([b[o], b[o + 1]]) as usize;
        let typ = b[o + 2];
        if len < 4 {
            if len == 0 {
                break;
            }
            return Err(format!("{}: bad GDS record length {} at byte {}", file, len, o));
        }
        if o + len > n {
            return Err(format!("{}: GDS record runs past the end of the file at byte {}", file, o));
        }
        let p = &b[o + 4..o + len];
        let i16v = |k: usize| i16::from_be_bytes([p[k], p[k + 1]]);
        let i32v = |k: usize| i32::from_be_bytes([p[k], p[k + 1], p[k + 2], p[k + 3]]);
        match typ {
            0x02 => lib.name = s(p),
            0x03 => {
                lib.user_per_db = r8(&p[0..8]);
                lib.meters_per_db = r8(&p[8..16]);
            }
            0x05 => cell = Some(Cell::default()),
            0x06 => {
                if let Some(c) = cell.as_mut() {
                    c.name = s(p);
                }
            }
            0x07 => {
                if let Some(c) = cell.take() {
                    lib.order.push(c.name.clone());
                    lib.cells.insert(c.name.clone(), c);
                }
            }
            0x08 | 0x2d => el = El::Poly(0, 0, vec![]),
            0x09 => el = El::Path(PathEl::default()),
            0x0a => el = El::Ref(Ref { mag: 1.0, cols: 1, rows: 1, ..Default::default() }),
            0x0b => el = El::Ref(Ref { aref: true, mag: 1.0, cols: 1, rows: 1, ..Default::default() }),
            0x0c => el = El::Text(0, 0, String::new(), vec![]),
            0x0d => match &mut el {
                El::Poly(l, _, _) | El::Text(l, _, _, _) => *l = i16v(0),
                El::Path(pa) => pa.layer = i16v(0),
                _ => {}
            },
            0x0e | 0x16 | 0x2e => match &mut el {
                El::Poly(_, d, _) | El::Text(_, d, _, _) => *d = i16v(0),
                El::Path(pa) => pa.dt = i16v(0),
                _ => {}
            },
            0x0f => {
                if let El::Path(pa) = &mut el {
                    pa.width = i32v(0);
                }
            }
            0x21 => {
                if let El::Path(pa) = &mut el {
                    pa.pathtype = i16v(0);
                }
            }
            0x30 => {
                if let El::Path(pa) = &mut el {
                    pa.bgn = i32v(0);
                }
            }
            0x31 => {
                if let El::Path(pa) = &mut el {
                    pa.end = i32v(0);
                }
            }
            0x12 => {
                if let El::Ref(r) = &mut el {
                    r.sname = s(p);
                }
            }
            0x13 => {
                if let El::Ref(r) = &mut el {
                    r.cols = i16v(0) as i32;
                    r.rows = i16v(2) as i32;
                }
            }
            0x19 => {
                if let El::Text(_, _, st, _) = &mut el {
                    *st = s(p);
                }
            }
            0x1a => {
                if let El::Ref(r) = &mut el {
                    r.reflect = u16::from_be_bytes([p[0], p[1]]) & 0x8000 != 0;
                }
            }
            0x1b => {
                if let El::Ref(r) = &mut el {
                    r.mag = r8(&p[0..8]);
                }
            }
            0x1c => {
                if let El::Ref(r) = &mut el {
                    r.angle = r8(&p[0..8]);
                }
            }
            0x10 => {
                let xy: Vec<i32> = (0..p.len() / 4).map(|k| i32v(4 * k)).collect();
                match &mut el {
                    El::Poly(_, _, v) | El::Text(_, _, _, v) => *v = xy,
                    El::Path(pa) => pa.xy = xy,
                    El::Ref(r) => r.xy = xy,
                    El::None => {}
                }
            }
            0x11 => {
                if let Some(c) = cell.as_mut() {
                    match std::mem::replace(&mut el, El::None) {
                        El::Poly(l, d, mut xy) => {
                            let k = xy.len();
                            if k >= 4 && xy[0] == xy[k - 2] && xy[1] == xy[k - 1] {
                                xy.truncate(k - 2);
                            }
                            c.polys.push(Poly { layer: l, dt: d, pts: xy });
                        }
                        El::Path(pa) => c.paths.push(pa),
                        El::Text(l, d, st, xy) => c.texts.push(Text { layer: l, dt: d, string: st, x: xy.first().copied().unwrap_or(0), y: xy.get(1).copied().unwrap_or(0), depth: 0 }),
                        El::Ref(r) => c.refs.push(r),
                        El::None => {}
                    }
                }
                el = El::None;
            }
            0x04 => break,
            _ => {}
        }
        o += len;
    }
    Ok(lib)
}

pub fn top_cells(lib: &GdsLib) -> Vec<String> {
    let used: std::collections::HashSet<&String> = lib.cells.values().flat_map(|c| c.refs.iter().map(|r| &r.sname)).collect();
    lib.order.iter().filter(|n| !used.contains(n)).cloned().collect()
}

fn path_polys(pa: &PathEl) -> Vec<Vec<i32>> {
    let mut out = vec![];
    let hw = (pa.width.abs() as f64) / 2.0;
    let ext0 = match pa.pathtype {
        2 => hw,
        4 => pa.bgn as f64,
        _ => 0.0,
    };
    let ext1 = match pa.pathtype {
        2 => hw,
        4 => pa.end as f64,
        _ => 0.0,
    };
    let np = pa.xy.len() / 2;
    for i in 0..np.saturating_sub(1) {
        let (mut x1, mut y1, mut x2, mut y2) = (pa.xy[2 * i] as f64, pa.xy[2 * i + 1] as f64, pa.xy[2 * i + 2] as f64, pa.xy[2 * i + 3] as f64);
        let (dx, dy) = (x2 - x1, y2 - y1);
        let l = (dx * dx + dy * dy).sqrt();
        if l == 0.0 {
            continue;
        }
        let (ux, uy) = (dx / l, dy / l);
        let e1 = if i == 0 { ext0 } else { hw };
        let e2 = if i + 2 == np { ext1 } else { hw };
        x1 -= ux * e1;
        y1 -= uy * e1;
        x2 += ux * e2;
        y2 += uy * e2;
        let (nx, ny) = (-uy * hw, ux * hw);
        out.push([x1 + nx, y1 + ny, x2 + nx, y2 + ny, x2 - nx, y2 - ny, x1 - nx, y1 - ny].iter().map(|v| jr(*v)).collect());
    }
    out
}

#[derive(Clone, Debug, Default)]
pub struct Flat {
    pub polys: Vec<Poly>,
    pub texts: Vec<Text>,
    pub missing: Vec<String>,
    /// metres per database unit of the library (0 when unknown)
    pub dbu_m: f64,
}

type M = [f64; 6];
fn mul(p: &M, q: &M) -> M {
    [p[0] * q[0] + p[1] * q[2], p[0] * q[1] + p[1] * q[3], p[2] * q[0] + p[3] * q[2], p[2] * q[1] + p[3] * q[3], p[0] * q[4] + p[1] * q[5] + p[4], p[2] * q[4] + p[3] * q[5] + p[5]]
}
fn ref_matrix(r: &Ref, x: f64, y: f64) -> M {
    let t = r.angle.to_radians();
    let m = if r.mag == 0.0 { 1.0 } else { r.mag };
    let (mut c, mut s) = (t.cos(), t.sin());
    if c.abs() < 1e-12 {
        c = 0.0;
    }
    if s.abs() < 1e-12 {
        s = 0.0;
    }
    let f = if r.reflect { -1.0 } else { 1.0 };
    [m * c, -m * s * f, m * s, m * c * f, x, y]
}
fn tf(m: &M, pts: &[i32]) -> Vec<i32> {
    let mut o = Vec::with_capacity(pts.len());
    for k in 0..pts.len() / 2 {
        let (x, y) = (pts[2 * k] as f64, pts[2 * k + 1] as f64);
        o.push(jr(m[0] * x + m[1] * y + m[4]));
        o.push(jr(m[2] * x + m[3] * y + m[5]));
    }
    o
}

pub fn flatten(lib: &GdsLib, name: &str) -> Result<Flat, String> {
    let mut out = Flat { dbu_m: lib.meters_per_db, ..Default::default() };
    fn walk(lib: &GdsLib, name: &str, m: &M, depth: usize, out: &mut Flat) -> Result<(), String> {
        let c = match lib.cells.get(name) {
            Some(c) => c,
            None => {
                if !out.missing.contains(&name.to_string()) {
                    out.missing.push(name.to_string());
                }
                return Ok(());
            }
        };
        if depth > 64 {
            return Err(format!("GDS hierarchy deeper than 64 at {} (recursive reference?)", name));
        }
        for p in &c.polys {
            out.polys.push(Poly { layer: p.layer, dt: p.dt, pts: tf(m, &p.pts) });
        }
        for pa in &c.paths {
            for q in path_polys(pa) {
                out.polys.push(Poly { layer: pa.layer, dt: pa.dt, pts: tf(m, &q) });
            }
        }
        for t in &c.texts {
            let q = tf(m, &[t.x, t.y]);
            out.texts.push(Text { layer: t.layer, dt: t.dt, string: t.string.clone(), x: q[0], y: q[1], depth: depth as u16 });
        }
        for r in &c.refs {
            if !r.aref {
                walk(lib, &r.sname, &mul(m, &ref_matrix(r, r.xy[0] as f64, r.xy[1] as f64)), depth + 1, out)?;
            } else {
                let (x0, y0, xc, yc, xr, yr) = (r.xy[0] as f64, r.xy[1] as f64, r.xy[2] as f64, r.xy[3] as f64, r.xy[4] as f64, r.xy[5] as f64);
                for i in 0..r.cols {
                    for j in 0..r.rows {
                        let x = x0 + (xc - x0) * i as f64 / r.cols as f64 + (xr - x0) * j as f64 / r.rows as f64;
                        let y = y0 + (yc - y0) * i as f64 / r.cols as f64 + (yr - y0) * j as f64 / r.rows as f64;
                        walk(lib, &r.sname, &mul(m, &ref_matrix(r, x, y)), depth + 1, out)?;
                    }
                }
            }
        }
        Ok(())
    }
    walk(lib, name, &[1.0, 0.0, 0.0, 1.0, 0.0, 0.0], 0, &mut out)?;
    Ok(out)
}

pub fn bbox(polys: &[Poly]) -> [i32; 4] {
    let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for p in polys {
        for k in 0..p.pts.len() / 2 {
            b[0] = b[0].min(p.pts[2 * k]);
            b[1] = b[1].min(p.pts[2 * k + 1]);
            b[2] = b[2].max(p.pts[2 * k]);
            b[3] = b[3].max(p.pts[2 * k + 1]);
        }
    }
    if b[0] > b[2] { [0, 0, 0, 0] } else { b }
}

/// rectilinear (or any simple) polygon split into horizontal slabs: rectangles for filling
pub fn poly_rects(pts: &[i32]) -> Vec<[i32; 4]> {
    let k = pts.len() / 2;
    let mut ys: Vec<i32> = (0..k).map(|i| pts[2 * i + 1]).collect();
    ys.sort_unstable();
    ys.dedup();
    let mut out = vec![];
    for w in ys.windows(2) {
        let yc = (w[0] as f64 + w[1] as f64) / 2.0;
        let mut xs: Vec<f64> = vec![];
        for i in 0..k {
            let (x1, y1) = (pts[2 * i] as f64, pts[2 * i + 1] as f64);
            let (x2, y2) = (pts[(2 * i + 2) % (2 * k)] as f64, pts[(2 * i + 3) % (2 * k)] as f64);
            if (y1 <= yc) != (y2 <= yc) {
                xs.push(x1 + (yc - y1) * (x2 - x1) / (y2 - y1));
            }
        }
        xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        for p in xs.chunks(2) {
            if p.len() == 2 {
                out.push([p[0].round() as i32, w[0], p[1].round() as i32, w[1]]);
            }
        }
    }
    out
}
