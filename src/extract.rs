//! Layer maps, extraction rules and a grid-based device and net extractor for one flattened GDS cell
//! (port of the web viewer's extract.js). Geometry is put on a grid made from every vertex coordinate of
//! the layers the rules use, so boolean layer operations, overlaps and abutment are exact for rectilinear shapes.
use crate::gds::Flat;
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Default, Clone)]
pub struct LayerMap {
    pub by_name: HashMap<String, (i32, i32)>,
    pub by_num: HashMap<(i32, i32), String>,
}

fn is_uint(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// layermap ("name purpose layer datatype"), KLayout .lyp (XML), or "name L/D" / "name L D" lines
pub fn parse_layer_map(text: &str) -> LayerMap {
    let mut map = LayerMap::default();
    let mut add = |name: &str, purpose: &str, l: i32, d: i32| {
        let key = if !purpose.is_empty() && purpose != "drawing" { format!("{}.{}", name, purpose) } else { name.to_string() };
        map.by_name.entry(key.clone()).or_insert((l, d));
        if purpose == "drawing" || purpose.is_empty() {
            map.by_name.insert(name.to_string(), (l, d));
        }
        map.by_num.entry((l, d)).or_insert(key);
    };
    let low = text.to_lowercase();
    if low.contains("<layer-properties") {
        let mut pos = 0;
        while let Some(a) = low[pos..].find("<properties>") {
            let a = pos + a + "<properties>".len();
            let b = match low[a..].find("</properties>") {
                Some(b) => a + b,
                None => break,
            };
            let body = &text[a..b];
            let bl = &low[a..b];
            pos = b;
            let src = bl.find("<source>").and_then(|k| {
                let s = body[k + 8..].trim_start();
                let mut it = s.splitn(2, '/');
                let l: String = it.next()?.chars().take_while(|c| c.is_ascii_digit()).collect();
                let d: String = it.next()?.chars().take_while(|c| c.is_ascii_digit()).collect();
                Some((l.parse::<i32>().ok()?, d.parse::<i32>().ok()?))
            });
            let (l, d) = match src {
                Some(x) => x,
                None => continue,
            };
            let mut name = bl
                .find("<name>")
                .and_then(|k| body[k + 6..].find('<').map(|e| body[k + 6..k + 6 + e].trim().to_string()))
                .unwrap_or_default();
            // drop a trailing " L/D"
            if let Some(sp) = name.rfind(char::is_whitespace) {
                let tail = &name[sp + 1..];
                let mut it = tail.splitn(2, '/');
                if let (Some(x), Some(y)) = (it.next(), it.next()) {
                    if is_uint(x) && is_uint(y) {
                        name = name[..sp].trim().to_string();
                    }
                }
            }
            if name.is_empty() {
                name = format!("{}/{}", l, d);
            }
            add(&name, "", l, d);
        }
        return map;
    }
    for raw in text.lines() {
        let s = raw.split('#').next().unwrap().trim();
        if s.is_empty() {
            continue;
        }
        let t: Vec<&str> = s.split_whitespace().collect();
        let ld = |x: &str| -> Option<(i32, i32)> {
            let mut it = x.splitn(2, '/');
            let (a, b) = (it.next()?, it.next()?);
            if is_uint(a) && is_uint(b) { Some((a.parse().ok()?, b.parse().ok()?)) } else { None }
        };
        if t.len() >= 4 && is_uint(t[2]) && is_uint(t[3]) {
            add(t[0], &t[1].to_lowercase(), t[2].parse().unwrap_or(0), t[3].parse().unwrap_or(0));
        } else if t.len() >= 2 && ld(t[1]).is_some() {
            let (l, d) = ld(t[1]).unwrap();
            add(t[0], &t.get(2).unwrap_or(&"").to_lowercase(), l, d);
        } else if t.len() >= 3 && is_uint(t[1]) && is_uint(t[2]) {
            add(t[0], &t.get(3).unwrap_or(&"").to_lowercase(), t[1].parse().unwrap_or(0), t[2].parse().unwrap_or(0));
        }
    }
    map
}

#[derive(Clone, Debug, Default)]
pub struct DevRule {
    pub typ: String,
    pub channel: String,
    pub kv: HashMap<String, String>,
    pub line: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Rules {
    pub layers: Vec<(String, Vec<String>, usize)>,
    pub conduct: Vec<String>,
    pub connect: Vec<(String, String)>,
    /// abut A B: components of A and B that share an edge are one net (butted junctions under silicide)
    pub abut: Vec<(String, String)>,
    pub devices: Vec<DevRule>,
    pub labels: Vec<(String, String, usize)>,
    pub join_same_name: bool,
    /// metres per CDL length unit for w= and l= (1 = SI values such as 740n; 1e-6 when the CDL writes microns bare)
    pub cdl_scale: f64,
    pub errors: Vec<String>,
}

/// layer N = EXPR | conduct N ... | connect A B (A may be a virtual @node) | abut A B | device T CH gate=G sd=SD [when=L] [bulk=L|@name] [fin=F] [size=wl]
/// | label L/D -> N | join-same-name | cdl-scale F
/// size=wl measures each channel (W = half the channel edge shared with source/drain, L = area / W) and the
/// match then compares W (summed over parallel fingers) and L with the CDL w*m and l instead of fins.
pub fn parse_rules(text: &str) -> Rules {
    let mut r = Rules { cdl_scale: 1.0, ..Default::default() };
    for (k, raw) in text.lines().enumerate() {
        let ln = k + 1;
        let s = raw.split('#').next().unwrap().trim();
        if s.is_empty() {
            continue;
        }
        let t: Vec<&str> = s.split_whitespace().collect();
        let kw = t[0].to_lowercase();
        if kw == "layer" && t.get(2) == Some(&"=") {
            r.layers.push((t[1].to_string(), t[3..].iter().map(|x| x.to_string()).collect(), ln));
        } else if kw == "conduct" {
            r.conduct.extend(t[1..].iter().map(|x| x.to_string()));
        } else if kw == "connect" && t.len() == 3 {
            r.connect.push((t[1].to_string(), t[2].to_string()));
        } else if kw == "abut" && t.len() == 3 {
            r.abut.push((t[1].to_string(), t[2].to_string()));
        } else if kw == "device" && t.len() >= 3 {
            let mut d = DevRule { typ: t[1].to_string(), channel: t[2].to_string(), line: ln, ..Default::default() };
            for kv in &t[3..] {
                if let Some(p) = kv.find('=') {
                    if p > 0 {
                        d.kv.insert(kv[..p].to_string(), kv[p + 1..].to_string());
                    }
                }
            }
            if !d.kv.contains_key("gate") || !d.kv.contains_key("sd") {
                r.errors.push(format!("rules line {}: device needs gate= and sd=", ln));
            }
            if let Some(v) = d.kv.get("size") {
                if v != "wl" {
                    r.errors.push(format!("rules line {}: size= takes wl", ln));
                }
            }
            r.devices.push(d);
        } else if kw == "label" && t.get(2) == Some(&"->") && t.len() >= 4 {
            r.labels.push((t[1].to_string(), t[3].to_string(), ln));
        } else if kw == "join-same-name" {
            r.join_same_name = true;
        } else if kw == "cdl-scale" && t.len() == 2 && t[1].parse::<f64>().map_or(false, |v| v > 0.0) {
            r.cdl_scale = t[1].parse().unwrap();
        } else {
            r.errors.push(format!("rules line {}: not understood: {}", ln, s));
        }
    }
    r
}

/// built-in ASAP7 rules (moved to pdk.rs)
pub use crate::pdk::ASAP7_RULES;

#[derive(Clone, Debug, Default)]
pub struct ExNet {
    pub id: usize,
    pub names: Vec<String>,
    pub name: Option<String>,
    pub layers: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ExDevice {
    pub typ: String,
    pub cells: Vec<usize>,
    pub gate: i64,
    pub sd: Vec<usize>,
    pub bulk: i64,
    pub nfin: f64,
    pub fin_cells: usize,
    /// channel width and length in metres when the rule has size=wl, else 0
    pub w: f64,
    pub l: f64,
    /// channel (gate over active)
    pub rects: Vec<[i32; 4]>,
    /// the source and drain diffusion pieces touching the channel
    pub sd_rects: Vec<[i32; 4]>,
}

#[derive(Default)]
pub struct Extraction {
    pub nets: Vec<ExNet>,
    pub devices: Vec<ExDevice>,
    /// conductor layer -> net -> rectangles, in rule order
    pub shapes: Vec<(String, HashMap<usize, Vec<[i32; 4]>>)>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub nx: usize,
    pub ny: usize,
    pub prim: Vec<(String, (i32, i32))>,
    /// some device rule measures W/L (size=wl), so the match compares W/L
    pub size_wl: bool,
    /// metres per CDL length unit (rules cdl-scale)
    pub cdl_scale: f64,
}

fn lower_bound(a: &[f64], v: f64) -> usize {
    let (mut lo, mut hi) = (0usize, a.len().saturating_sub(1));
    while lo < hi {
        let m = (lo + hi) >> 1;
        if a[m] < v {
            lo = m + 1;
        } else {
            hi = m;
        }
    }
    lo
}

pub fn extract_cell(flat: &Flat, lm: &LayerMap, rules_text: &str) -> Result<Extraction, String> {
    let rules = parse_rules(rules_text);
    let mut errors = rules.errors.clone();
    let mut warnings = vec![];
    let derived: HashSet<&str> = rules.layers.iter().map(|l| l.0.as_str()).collect();
    let resolve = |nm: &str| -> Option<(i32, i32)> {
        let mut it = nm.splitn(2, '/');
        if let (Some(a), Some(b)) = (it.next(), it.next()) {
            if is_uint(a) && is_uint(b) {
                return Some((a.parse().ok()?, b.parse().ok()?));
            }
        }
        lm.by_name.get(nm).copied()
    };
    let mut prim: Vec<(String, (i32, i32))> = vec![];
    {
        let mut want = |nm: &str, errors: &mut Vec<String>| {
            if derived.contains(nm) || nm.starts_with('@') || prim.iter().any(|p| p.0 == nm) {
                return;
            }
            match resolve(nm) {
                Some(k) => prim.push((nm.to_string(), k)),
                None => errors.push(format!("layer {} is not in the layer map", nm)),
            }
        };
        for l in &rules.layers {
            for tk in &l.1 {
                let lw = tk.to_lowercase();
                if lw != "and" && lw != "or" && lw != "not" {
                    want(tk, &mut errors);
                }
            }
        }
        for c in &rules.conduct {
            want(c, &mut errors);
        }
        for (a, b) in rules.connect.iter().chain(&rules.abut) {
            want(a, &mut errors);
            want(b, &mut errors);
        }
        for d in &rules.devices {
            if !d.channel.starts_with('@') {
                want(&d.channel, &mut errors);
            }
            for k in ["gate", "sd", "when", "bulk", "fin"] {
                if let Some(v) = d.kv.get(k) {
                    if !v.starts_with('@') {
                        want(v, &mut errors);
                    }
                }
            }
        }
        for lb in &rules.labels {
            if !lb.1.starts_with('@') {
                want(&lb.1, &mut errors);
            }
        }
    }
    let mut by_key: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
    for (i, p) in flat.polys.iter().enumerate() {
        by_key.entry((p.layer as i32, p.dt as i32)).or_default().push(i);
    }
    let mut xs_set = BTreeSet::new();
    let mut ys_set = BTreeSet::new();
    for (_, k) in &prim {
        for &pi in by_key.get(k).map(|v| v.as_slice()).unwrap_or(&[]) {
            let p = &flat.polys[pi];
            for i in 0..p.pts.len() / 2 {
                xs_set.insert(p.pts[2 * i]);
                ys_set.insert(p.pts[2 * i + 1]);
            }
        }
    }
    let xs: Vec<f64> = xs_set.into_iter().map(|v| v as f64).collect();
    let ys: Vec<f64> = ys_set.into_iter().map(|v| v as f64).collect();
    let nx = xs.len().saturating_sub(1);
    let ny = ys.len().saturating_sub(1);
    let n = nx * ny;
    if n > 400_000_000 {
        return Err(format!("cell grid {}x{} is too large to extract", nx, ny));
    }
    let raster = |polys: &[usize]| -> Vec<u8> {
        let mut g = vec![0u8; n];
        for &pi in polys {
            let pts = &flat.polys[pi].pts;
            let k = pts.len() / 2;
            if k == 0 {
                continue;
            }
            let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
            for i in 0..k {
                let y = pts[2 * i + 1] as f64;
                ymin = ymin.min(y);
                ymax = ymax.max(y);
            }
            let (j0, j1) = (lower_bound(&ys, ymin), lower_bound(&ys, ymax));
            let mut cross: Vec<f64> = vec![];
            for j in j0..j1 {
                let yc = (ys[j] + ys[j + 1]) / 2.0;
                cross.clear();
                for i in 0..k {
                    let (x1, y1) = (pts[2 * i] as f64, pts[2 * i + 1] as f64);
                    let (x2, y2) = (pts[(2 * i + 2) % (2 * k)] as f64, pts[(2 * i + 3) % (2 * k)] as f64);
                    if (y1 <= yc) != (y2 <= yc) {
                        cross.push(x1 + (yc - y1) * (x2 - x1) / (y2 - y1));
                    }
                }
                cross.sort_by(|a, b| a.partial_cmp(b).unwrap());
                let mut q = 0;
                while q + 1 < cross.len() {
                    let (a, b) = (cross[q], cross[q + 1]);
                    let mut i = lower_bound(&xs, a);
                    while i < nx && (xs[i] + xs[i + 1]) / 2.0 < b {
                        if (xs[i] + xs[i + 1]) / 2.0 > a {
                            g[j * nx + i] = 1;
                        }
                        i += 1;
                    }
                    q += 2;
                }
            }
        }
        g
    };
    let mut grids: HashMap<String, Vec<u8>> = HashMap::new();
    for (nm, k) in &prim {
        grids.insert(nm.clone(), raster(by_key.get(k).map(|v| v.as_slice()).unwrap_or(&[])));
    }
    for (name, expr, line) in &rules.layers {
        let mut acc: Option<Vec<u8>> = None;
        let mut op = "or".to_string();
        for tk in expr {
            let lw = tk.to_lowercase();
            if lw == "and" || lw == "or" || lw == "not" {
                op = lw;
                continue;
            }
            let g = match grids.get(tk) {
                Some(g) => g,
                None => {
                    errors.push(format!("rules line {}: {} is not defined before use", line, tk));
                    continue;
                }
            };
            match acc.as_mut() {
                None => acc = Some(g.clone()),
                Some(a) => match op.as_str() {
                    "and" => a.iter_mut().zip(g).for_each(|(x, y)| *x &= y),
                    "or" => a.iter_mut().zip(g).for_each(|(x, y)| *x |= y),
                    _ => a.iter_mut().zip(g).for_each(|(x, y)| *x &= y ^ 1),
                },
            }
        }
        grids.insert(name.clone(), acc.unwrap_or_else(|| vec![0u8; n]));
    }
    // conductor components
    let mut parent: Vec<usize> = vec![];
    let mut node_layer: Vec<String> = vec![];
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
    let mut virt: HashMap<String, usize> = HashMap::new();
    let mut comp: HashMap<String, Vec<i32>> = HashMap::new();
    for nm in &rules.conduct {
        let g = match grids.get(nm) {
            Some(g) => g,
            None => {
                errors.push(format!("conduct {}: layer not defined", nm));
                continue;
            }
        };
        let mut c = vec![-1i32; n];
        let mut stack = vec![];
        for s in 0..n {
            if g[s] == 0 || c[s] >= 0 {
                continue;
            }
            let id = parent.len();
            parent.push(id);
            node_layer.push(nm.clone());
            c[s] = id as i32;
            stack.push(s);
            while let Some(q) = stack.pop() {
                let (i, j) = (q % nx, q / nx);
                let visit = |r: usize, c: &mut Vec<i32>, stack: &mut Vec<usize>| {
                    if g[r] != 0 && c[r] < 0 {
                        c[r] = id as i32;
                        stack.push(r);
                    }
                };
                if i > 0 {
                    visit(q - 1, &mut c, &mut stack);
                }
                if i + 1 < nx {
                    visit(q + 1, &mut c, &mut stack);
                }
                if j > 0 {
                    visit(q - nx, &mut c, &mut stack);
                }
                if j + 1 < ny {
                    visit(q + nx, &mut c, &mut stack);
                }
            }
        }
        comp.insert(nm.clone(), c);
    }
    for (a, b) in &rules.connect {
        // connect @node L: one virtual node (the substrate) joined to every component of conductor L
        if a.starts_with('@') || b.starts_with('@') {
            let (v, l) = if a.starts_with('@') { (a, b) } else { (b, a) };
            match comp.get(l) {
                Some(cl) => {
                    let id = *virt.entry(v.clone()).or_insert_with(|| {
                        let id = parent.len();
                        parent.push(id);
                        node_layer.push(v.clone());
                        id
                    });
                    for s in 0..n {
                        if cl[s] >= 0 {
                            unite(&mut parent, id, cl[s] as usize);
                        }
                    }
                }
                None => errors.push(format!("connect {} {}: {} must be listed in conduct", a, b, l)),
            }
            continue;
        }
        match (comp.get(a), comp.get(b)) {
            (Some(ca), Some(cb)) => {
                for s in 0..n {
                    if ca[s] >= 0 && cb[s] >= 0 {
                        unite(&mut parent, ca[s] as usize, cb[s] as usize);
                    }
                }
            }
            _ => errors.push(format!("connect {} {}: both layers must be listed in conduct", a, b)),
        }
    }
    for (a, b) in &rules.abut {
        match (comp.get(a), comp.get(b)) {
            (Some(ca), Some(cb)) => {
                for s in 0..n {
                    if ca[s] < 0 {
                        continue;
                    }
                    let (i, j) = (s % nx, s / nx);
                    let mut touch = |r: usize| {
                        if cb[r] >= 0 {
                            unite(&mut parent, ca[s] as usize, cb[r] as usize);
                        }
                    };
                    if i > 0 {
                        touch(s - 1);
                    }
                    if i + 1 < nx {
                        touch(s + 1);
                    }
                    if j > 0 {
                        touch(s - nx);
                    }
                    if j + 1 < ny {
                        touch(s + nx);
                    }
                }
            }
            _ => errors.push(format!("abut {} {}: both layers must be listed in conduct", a, b)),
        }
    }
    let nbrs = |q: usize| -> [Option<usize>; 4] {
        let (i, j) = (q % nx, q / nx);
        [if i > 0 { Some(q - 1) } else { None }, if i + 1 < nx { Some(q + 1) } else { None }, if j > 0 { Some(q - nx) } else { None }, if j + 1 < ny { Some(q + nx) } else { None }]
    };
    // devices
    struct RawDev {
        typ: String,
        cells: Vec<usize>,
        gates: Vec<usize>,
        sds: Vec<usize>,
        bulks: Vec<usize>,
        bulk_virtual: Option<String>,
        sd_layer: String,
        nfin: f64,
        fin_cells: usize,
        w: f64,
        l: f64,
    }
    let dbu = if flat.dbu_m > 0.0 { flat.dbu_m } else { 1e-9 };
    let mut raw: Vec<RawDev> = vec![];
    for dr in &rules.devices {
        let gname = dr.kv.get("gate").cloned().unwrap_or_default();
        let sname = dr.kv.get("sd").cloned().unwrap_or_default();
        let (ch, gc, sdc) = match (grids.get(&dr.channel), comp.get(&gname), comp.get(&sname)) {
            (Some(a), Some(b), Some(c)) => (a, b, c),
            _ => {
                errors.push(format!("device {}: channel, gate and sd layers must exist (gate and sd as conductors)", dr.typ));
                continue;
            }
        };
        let when = dr.kv.get("when").and_then(|w| grids.get(w));
        let fin = dr.kv.get("fin").and_then(|w| grids.get(w));
        let bulk_v = dr.kv.get("bulk").cloned();
        let bulk_c = bulk_v.as_ref().filter(|b| !b.starts_with('@')).and_then(|b| comp.get(b));
        let size_wl = dr.kv.get("size").map_or(false, |v| v == "wl");
        let mut seen = vec![0u8; n];
        let ok = |r: usize| ch[r] != 0 && when.map_or(true, |w| w[r] != 0);
        for s in 0..n {
            if ch[s] == 0 || seen[s] != 0 {
                continue;
            }
            if let Some(w) = when {
                if w[s] == 0 {
                    continue;
                }
            }
            let mut cells = vec![];
            let mut stack = vec![s];
            seen[s] = 1;
            while let Some(q) = stack.pop() {
                cells.push(q);
                for r in nbrs(q).into_iter().flatten() {
                    if ok(r) && seen[r] == 0 {
                        seen[r] = 1;
                        stack.push(r);
                    }
                }
            }
            let in_ch: HashSet<usize> = cells.iter().copied().collect();
            let (mut gates, mut sds, mut bulks) = (vec![], vec![], vec![]);
            let push_u = |v: &mut Vec<usize>, x: usize| {
                if !v.contains(&x) {
                    v.push(x)
                }
            };
            let mut fin_cells = 0;
            // W/L: channel edge length shared with source/drain pieces, and channel area (database units)
            let (mut sd_edge, mut area) = (0f64, 0f64);
            for &q in &cells {
                if size_wl {
                    let (i, j) = (q % nx, q / nx);
                    let (dx, dy) = (xs[i + 1] - xs[i], ys[j + 1] - ys[j]);
                    area += dx * dy;
                    for (k, r) in nbrs(q).into_iter().enumerate() {
                        if let Some(r) = r {
                            if !in_ch.contains(&r) && sdc[r] >= 0 {
                                sd_edge += if k < 2 { dy } else { dx };
                            }
                        }
                    }
                }
                if gc[q] >= 0 {
                    push_u(&mut gates, gc[q] as usize);
                }
                if let Some(bc) = bulk_c {
                    if bc[q] >= 0 {
                        push_u(&mut bulks, bc[q] as usize);
                    }
                }
                if let Some(f) = fin {
                    if f[q] != 0 {
                        fin_cells += 1;
                    }
                }
                for r in nbrs(q).into_iter().flatten() {
                    if !in_ch.contains(&r) && sdc[r] >= 0 {
                        push_u(&mut sds, sdc[r] as usize);
                    }
                }
            }
            // fins: distinct vertical runs of fin cells per channel column
            let mut nfin = 0usize;
            if let Some(f) = fin {
                let mut rows: HashMap<usize, Vec<usize>> = HashMap::new();
                for &q in &cells {
                    if f[q] != 0 {
                        rows.entry(q % nx).or_default().push(q / nx);
                    }
                }
                for js in rows.values_mut() {
                    js.sort_unstable();
                    let mut runs = 0;
                    for k in 0..js.len() {
                        if k == 0 || js[k] != js[k - 1] + 1 {
                            runs += 1;
                        }
                    }
                    nfin = nfin.max(runs);
                }
            }
            let (w, l) = if size_wl && sd_edge > 0.0 { (sd_edge / 2.0 * dbu, area / (sd_edge / 2.0) * dbu) } else { (0.0, 0.0) };
            raw.push(RawDev { typ: dr.typ.clone(), cells, gates, sds, bulks, bulk_virtual: bulk_v.clone().filter(|b| b.starts_with('@')), sd_layer: sname.clone(), nfin: nfin as f64, fin_cells, w, l });
        }
    }
    // labels
    let mut texts: Vec<(usize, String)> = vec![];
    for (src, to, _) in &rules.labels {
        let k = match resolve(src) {
            Some(k) => k,
            None => {
                errors.push(format!("label {}: not a layer", src));
                continue;
            }
        };
        for t in &flat.texts {
            // only the cell's own texts name nets: a placed sub-cell's pin texts are local to that instance
            if (t.layer as i32, t.dt as i32) != k || t.depth > 0 {
                continue;
            }
            let mut node: i64 = -1;
            if to.starts_with('@') {
                let id = *virt.entry(to.clone()).or_insert_with(|| {
                    let id = parent.len();
                    parent.push(id);
                    node_layer.push(to.clone());
                    id
                });
                node = id as i64;
            } else {
                let c = match comp.get(to) {
                    Some(c) => c,
                    None => {
                        errors.push(format!("label {} -> {}: {} must be a conductor", src, to, to));
                        break;
                    }
                };
                let (tx, ty) = (t.x as f64, t.y as f64);
                let (i0, j0) = (lower_bound(&xs, tx) as i64, lower_bound(&ys, ty) as i64);
                for (i, j) in [(i0, j0), (i0 - 1, j0), (i0, j0 - 1), (i0 - 1, j0 - 1)] {
                    if i < 0 || j < 0 || i >= nx as i64 || j >= ny as i64 {
                        continue;
                    }
                    let (i, j) = (i as usize, j as usize);
                    if tx < xs[i] || tx > xs[i + 1] || ty < ys[j] || ty > ys[j + 1] {
                        continue;
                    }
                    if c[j * nx + i] >= 0 {
                        node = c[j * nx + i] as i64;
                        break;
                    }
                }
            }
            if node < 0 {
                warnings.push(format!("label \"{}\" on {} is not over {}", t.string, src, to));
                continue;
            }
            texts.push((node as usize, t.string.clone()));
        }
    }
    if rules.join_same_name {
        let mut first: HashMap<&str, usize> = HashMap::new();
        for (node, name) in &texts {
            match first.get(name.as_str()) {
                Some(&f) => unite(&mut parent, f, *node),
                None => {
                    first.insert(name, *node);
                }
            }
        }
    }
    for d in raw.iter_mut() {
        if let Some(bv) = d.bulk_virtual.clone() {
            let id = *virt.entry(bv.clone()).or_insert_with(|| {
                let id = parent.len();
                parent.push(id);
                node_layer.push(bv.clone());
                id
            });
            d.bulks = vec![id];
        }
    }
    // nets
    let mut net_of: HashMap<usize, usize> = HashMap::new();
    let mut next_id = 0usize;
    let mut net = |node: usize, parent: &mut Vec<usize>| -> usize {
        let r = find(parent, node);
        *net_of.entry(r).or_insert_with(|| {
            next_id += 1;
            next_id - 1
        })
    };
    let mut name_sets: Vec<BTreeSet<String>> = vec![];
    let mut layer_sets: Vec<Vec<String>> = vec![];
    for a in 0..parent.len() {
        let id = net(a, &mut parent);
        if layer_sets.len() <= id {
            layer_sets.resize(id + 1, vec![]);
            name_sets.resize(id + 1, BTreeSet::new());
        }
        if !layer_sets[id].contains(&node_layer[a]) {
            layer_sets[id].push(node_layer[a].clone());
        }
    }
    for (node, name) in &texts {
        let id = net(*node, &mut parent);
        name_sets[id].insert(name.clone());
    }
    let nets: Vec<ExNet> = (0..layer_sets.len())
        .map(|i| {
            let names: Vec<String> = name_sets[i].iter().cloned().collect();
            ExNet { id: i, name: names.first().cloned(), names, layers: layer_sets[i].clone() }
        })
        .collect();
    let cell_xy = |q: usize| format!("({}, {})", xs[q % nx], ys[q / nx]);
    let mut devices = vec![];
    let mut dev_sd: Vec<(String, Vec<usize>)> = vec![];
    for d in raw {
        dev_sd.push((d.sd_layer.clone(), d.sds.clone()));
        let uniq = |v: &[usize], parent: &mut Vec<usize>, net: &mut dyn FnMut(usize, &mut Vec<usize>) -> usize| {
            let mut o: Vec<usize> = vec![];
            for &x in v {
                let id = net(x, parent);
                if !o.contains(&id) {
                    o.push(id);
                }
            }
            o
        };
        let g = uniq(&d.gates, &mut parent, &mut net);
        let mut sd = uniq(&d.sds, &mut parent, &mut net);
        if sd.len() == 1 && d.sds.len() == 2 {
            // two separate diffusion pieces wired to one net (a decap or a tie device): source = drain
            sd.push(sd[0]);
        }
        let b = uniq(&d.bulks, &mut parent, &mut net);
        if g.len() != 1 {
            warnings.push(format!("{} at {}: {} gate nets", d.typ, cell_xy(d.cells[0]), g.len()));
        }
        if sd.len() != 2 {
            warnings.push(format!("{} at {}: {} source/drain nets", d.typ, cell_xy(d.cells[0]), sd.len()));
        }
        devices.push(ExDevice { typ: d.typ, gate: if g.len() == 1 { g[0] as i64 } else { -1 }, sd, bulk: if b.len() == 1 { b[0] as i64 } else { -1 }, nfin: d.nfin, fin_cells: d.fin_cells, w: d.w, l: d.l, cells: d.cells, rects: vec![], sd_rects: vec![] });
    }
    // highlight geometry
    let xi = |v: f64| v as i32;
    let mut shapes = vec![];
    for nm in &rules.conduct {
        let mut by_net: HashMap<usize, Vec<[i32; 4]>> = HashMap::new();
        if let Some(c) = comp.get(nm) {
            for j in 0..ny {
                let mut i = 0;
                while i < nx {
                    let s = j * nx + i;
                    if c[s] < 0 {
                        i += 1;
                        continue;
                    }
                    let nn = net(c[s] as usize, &mut parent);
                    let a = i;
                    while i < nx && c[j * nx + i] >= 0 && net(c[j * nx + i] as usize, &mut parent) == nn {
                        i += 1;
                    }
                    by_net.entry(nn).or_default().push([xi(xs[a]), xi(ys[j]), xi(xs[i]), xi(ys[j + 1])]);
                }
            }
            for r in by_net.values_mut() {
                merge_rows(r);
            }
        }
        shapes.push((nm.clone(), by_net));
    }
    for d in devices.iter_mut() {
        let set: HashSet<usize> = d.cells.iter().copied().collect();
        let mut out = vec![];
        for j in 0..ny {
            let mut i = 0;
            while i < nx {
                if !set.contains(&(j * nx + i)) {
                    i += 1;
                    continue;
                }
                let a = i;
                while i < nx && set.contains(&(j * nx + i)) {
                    i += 1;
                }
                out.push([xi(xs[a]), xi(ys[j]), xi(xs[i]), xi(ys[j + 1])]);
            }
        }
        merge_rows(&mut out);
        d.rects = out;
    }
    // source/drain pieces: rectangles of each component of the device's sd layer, once per layer
    let mut comp_rects: HashMap<String, HashMap<i32, Vec<[i32; 4]>>> = HashMap::new();
    for (layer, _) in &dev_sd {
        if comp_rects.contains_key(layer) {
            continue;
        }
        let mut by: HashMap<i32, Vec<[i32; 4]>> = HashMap::new();
        if let Some(c) = comp.get(layer) {
            for j in 0..ny {
                let mut i = 0;
                while i < nx {
                    let id = c[j * nx + i];
                    if id < 0 {
                        i += 1;
                        continue;
                    }
                    let a = i;
                    while i < nx && c[j * nx + i] == id {
                        i += 1;
                    }
                    by.entry(id).or_default().push([xi(xs[a]), xi(ys[j]), xi(xs[i]), xi(ys[j + 1])]);
                }
            }
            for r in by.values_mut() {
                merge_rows(r);
            }
        }
        comp_rects.insert(layer.clone(), by);
    }
    for (d, (layer, ids)) in devices.iter_mut().zip(&dev_sd) {
        let by = &comp_rects[layer];
        for id in ids {
            if let Some(r) = by.get(&(*id as i32)) {
                d.sd_rects.extend_from_slice(r);
            }
        }
    }
    let size_wl = rules.devices.iter().any(|d| d.kv.get("size").map_or(false, |v| v == "wl"));
    Ok(Extraction { nets, devices, shapes, errors, warnings, nx, ny, prim, size_wl, cdl_scale: rules.cdl_scale })
}

/// merge vertically adjacent row runs with the same x span
fn merge_rows(rects: &mut Vec<[i32; 4]>) {
    rects.sort_by(|a, b| a[0].cmp(&b[0]).then(a[2].cmp(&b[2])).then(a[1].cmp(&b[1])));
    let mut out: Vec<[i32; 4]> = vec![];
    for r in rects.iter() {
        if let Some(last) = out.last_mut() {
            if last[0] == r[0] && last[2] == r[2] && last[3] == r[1] {
                last[3] = r[3];
                continue;
            }
        }
        out.push(*r);
    }
    *rects = out;
}
