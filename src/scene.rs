//! Drawable scene built from a Layout: instances sorted by draw slot, layer styles, a picking grid and
//! per-net shape lists.
use crate::def::Layout;
use crate::lef::Lib;
use crate::{K_BLOCK, K_IOPIN, K_SVIA, K_SWIRE, K_VIA, K_WIRE};
use bytemuck::{Pod, Zeroable};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

/// one rectangle on the GPU: integer coordinates and its style slot
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, Default)]
pub struct Inst {
    pub r: [i32; 4],
    pub slot: u32,
}

pub const GROUP_SIGNAL: usize = 0;
pub const GROUP_POWER: usize = 1;
pub const GROUP_PIN: usize = 2;
pub const GROUPS: usize = 3;
/// slot 0 holds the component boxes, then layer * GROUPS + group + 1
pub const SLOT_CELLS: u32 = 0;
pub fn slot_of(layer: u8, group: usize) -> u32 {
    1 + layer as u32 * GROUPS as u32 + group as u32
}

#[derive(Clone, Debug)]
pub struct LayerStyle {
    pub name: String,
    pub color: [f32; 3],
    pub cut: bool,
    pub hatch: u32, // 0 none, 1 '/', 2 '\'
    pub visible: bool,
    pub count: usize,
}

pub const METAL: [u32; 10] = [0x1971c2, 0xe67700, 0x2f9e44, 0xc2255c, 0x7048e8, 0x0c8599, 0xa0522d, 0x74b816, 0x5c7cfa, 0x868e96];
pub fn rgb(h: u32) -> [f32; 3] {
    [((h >> 16) & 255) as f32 / 255.0, ((h >> 8) & 255) as f32 / 255.0, (h & 255) as f32 / 255.0]
}

pub fn layer_styles(lay: &Layout, lib: &Lib, dark: bool) -> Vec<LayerStyle> {
    let mut out = vec![];
    let mut order = 0usize;
    for nm in &lay.layer_names {
        let ty = lib.layers.get(nm).map(|l| l.kind.clone()).unwrap_or_default();
        let cut = ty == "CUT" || (ty.is_empty() && nm.len() > 1 && (nm.starts_with('V') || nm.starts_with('v')) && nm[1..].chars().all(|c| c.is_ascii_digit()));
        let color = if cut {
            if dark { [0.81, 0.83, 0.85] } else { [0.20, 0.23, 0.25] }
        } else {
            let mi = nm.strip_prefix('M').or_else(|| nm.strip_prefix("metal")).and_then(|d| d.parse::<usize>().ok()).map(|v| v.saturating_sub(1)).unwrap_or(order);
            rgb(METAL[mi % METAL.len()])
        };
        let hatch = if cut { 0 } else if order % 2 == 0 { 1 } else { 2 };
        if !cut {
            order += 1;
        }
        out.push(LayerStyle { name: nm.clone(), color, cut, hatch, visible: nm != "(placement)", count: 0 });
    }
    out
}

pub struct Scene {
    pub inst: Vec<Inst>,
    /// with `build_lean`: the rectangle drawn at each position after the component boxes (instead of `inst`)
    pub order: Vec<u32>,
    /// with `build_lean`: the component drawn at each of the first positions
    pub corder: Vec<u32>,
    /// with `build_lean` on large layouts: instance ranges by tile, and the zoomed-out overview
    pub tiles: Option<Tiles>,
    pub overview: Option<Overview>,
    /// slot -> [start, end) in inst
    pub slot_range: Vec<(u32, u32)>,
    pub styles: Vec<LayerStyle>,
    pub die: [i32; 4],
    pub grid: Grid,
    pub net_start: Vec<u32>,
    pub net_items: Vec<u32>,
    pub comp_net_pins: CompPins,
}

/// pin -> net of every component: `comp_net_pins[ci]` lists (pin, net) of component ci; pin names are shared
#[derive(Default)]
pub struct CompPins {
    start: Vec<u32>,
    items: Vec<(Arc<str>, u32)>,
}
impl std::ops::Index<usize> for CompPins {
    type Output = [(Arc<str>, u32)];
    fn index(&self, ci: usize) -> &Self::Output {
        &self.items[self.start[ci] as usize..self.start[ci + 1] as usize]
    }
}
impl CompPins {
    pub fn len(&self) -> usize {
        self.start.len().saturating_sub(1)
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// entries over all components (for memory reports)
    pub fn entries(&self) -> usize {
        self.items.len()
    }
}

/// layouts with at least this many instances are drawn by tile and with a coarse overview when zoomed out
pub const LARGE: usize = 4 << 20;
/// tiles per side of the die for drawing only what is in view
pub const TILES: usize = 64;
/// buckets per slot: 0 holds shapes larger than a tile or outside the die, then the tiles row by row
pub const BUCKETS: usize = TILES * TILES + 1;
/// overview bins per side of the die
pub const OV_BINS: usize = 2048;

/// where the instances of each slot are, by tile (large layouts only)
#[derive(Clone, Debug, Default)]
pub struct Tiles {
    pub x0: i64,
    pub y0: i64,
    pub tw: i64,
    pub th: i64,
    /// start[s * BUCKETS + b]: first instance of bucket b of slot s; the last entry is the total
    pub start: Vec<u32>,
}

impl Tiles {
    fn bucket(&self, b: [i32; 4]) -> usize {
        let (x1, y1) = (b[0] as i64 - self.x0, b[1] as i64 - self.y0);
        if x1 < 0 || y1 < 0 || (b[2] - b[0]) as i64 > self.tw || (b[3] - b[1]) as i64 > self.th {
            return 0;
        }
        let (tx, ty) = ((x1 / self.tw) as usize, (y1 / self.th) as usize);
        if tx >= TILES || ty >= TILES {
            return 0;
        }
        1 + ty * TILES + tx
    }

    /// instance ranges of slot s that can touch the world box v (x1, y1, x2, y2)
    pub fn ranges(&self, s: usize, v: [f64; 4], out: &mut Vec<(u32, u32)>) {
        let base = s * BUCKETS;
        out.push((self.start[base], self.start[base + 1]));
        // a shape starts in its tile and reaches at most one tile further right and up
        let t = |v: f64, o: i64, w: i64| ((v - o as f64) / w as f64).floor();
        let (tx0, tx1) = ((t(v[0], self.x0, self.tw) - 1.0).max(0.0), t(v[2], self.x0, self.tw).min(TILES as f64 - 1.0));
        let (ty0, ty1) = ((t(v[1], self.y0, self.th) - 1.0).max(0.0), t(v[3], self.y0, self.th).min(TILES as f64 - 1.0));
        if tx1 < tx0 || ty1 < ty0 {
            return;
        }
        let (tx0, tx1, ty0, ty1) = (tx0 as usize, tx1 as usize, ty0 as usize, ty1 as usize);
        if tx0 == 0 && tx1 == TILES - 1 {
            out.push((self.start[base + 1 + ty0 * TILES], self.start[base + 1 + (ty1 + 1) * TILES]));
        } else {
            for ty in ty0..=ty1 {
                out.push((self.start[base + 1 + ty * TILES + tx0], self.start[base + 1 + ty * TILES + tx1 + 1]));
            }
        }
    }
}

/// coarse copy of every slot, drawn when the view is zoomed so far out that a bin is about a pixel:
/// covered bins merged into rectangles (runs along x, equal runs of following rows joined)
#[derive(Clone, Debug, Default)]
pub struct Overview {
    /// world size of one bin
    pub bin: f64,
    pub inst: Vec<Inst>,
    /// slot -> [start, end) in inst
    pub slot_range: Vec<(u32, u32)>,
}

/// first style index of the overview styles (after the slots and the four extra styles of gpu::styles_for)
pub fn overview_style_base(nslots: usize) -> u32 {
    nslots as u32 + 4
}

fn overview_of(rects: impl Iterator<Item = [i32; 4]>, x0: i64, y0: i64, bw: i64, bh: i64, slot: u32, out: &mut Vec<Inst>) {
    let n = OV_BINS;
    let words = n / 64;
    let mut bits = vec![0u64; n * words];
    for b in rects {
        let cx = |x: i32| (((x as i64 - x0) / bw).clamp(0, n as i64 - 1)) as usize;
        let cy = |y: i32| (((y as i64 - y0) / bh).clamp(0, n as i64 - 1)) as usize;
        let (i0, i1, j0, j1) = (cx(b[0]), cx((b[2] - 1).max(b[0])), cy(b[1]), cy((b[3] - 1).max(b[1])));
        for j in j0..=j1 {
            let row = &mut bits[j * words..(j + 1) * words];
            for i in i0..=i1 {
                row[i / 64] |= 1u64 << (i % 64);
            }
        }
    }
    let rect = |a: usize, b: usize, ya: usize, yb: usize| Inst { r: [(x0 + a as i64 * bw) as i32, (y0 + ya as i64 * bh) as i32, (x0 + b as i64 * bw) as i32, (y0 + yb as i64 * bh) as i32], slot };
    // open: runs (x start, x end, first row) of the previous row
    let mut open: Vec<(usize, usize, usize)> = vec![];
    let mut next: Vec<(usize, usize, usize)> = vec![];
    for j in 0..=n {
        let mut runs: Vec<(usize, usize)> = vec![];
        if j < n {
            let row = &bits[j * words..(j + 1) * words];
            let mut i = 0;
            while i < n {
                if row[i / 64] >> (i % 64) & 1 == 1 {
                    let a = i;
                    while i < n && row[i / 64] >> (i % 64) & 1 == 1 {
                        i += 1;
                    }
                    runs.push((a, i));
                } else {
                    i += 1;
                }
            }
        }
        // join runs equal to an open run, close the others
        let mut k = 0;
        next.clear();
        for &(a, b) in &runs {
            while k < open.len() && open[k].0 < a {
                out.push(rect(open[k].0, open[k].1, open[k].2, j));
                k += 1;
            }
            if k < open.len() && open[k].0 == a && open[k].1 == b {
                next.push(open[k]);
                k += 1;
            } else {
                if k < open.len() && open[k].0 == a {
                    out.push(rect(open[k].0, open[k].1, open[k].2, j));
                    k += 1;
                }
                next.push((a, b, j));
            }
        }
        while k < open.len() {
            out.push(rect(open[k].0, open[k].1, open[k].2, j));
            k += 1;
        }
        std::mem::swap(&mut open, &mut next);
    }
}

/// draw group of a shape kind; None for blockages (not drawn)
pub fn group_of(k: u8) -> Option<usize> {
    match k {
        K_WIRE | K_VIA => Some(GROUP_SIGNAL),
        K_SWIRE | K_SVIA => Some(GROUP_POWER),
        K_IOPIN => Some(GROUP_PIN),
        K_BLOCK => None,
        _ => Some(GROUP_SIGNAL),
    }
}

/// box of a component as drawn
pub fn comp_box(c: &crate::def::Comp) -> [i32; 4] {
    [c.x.round() as i32, c.y.round() as i32, (c.x + c.w).round() as i32, (c.y + c.h).round() as i32]
}

/// uniform grid over rectangle indices and component indices
pub struct Grid {
    pub x0: i64,
    pub y0: i64,
    pub cw: i64,
    pub ch: i64,
    pub nx: usize,
    pub ny: usize,
    pub start: Vec<u32>,
    pub items: Vec<u32>,
    pub cstart: Vec<u32>,
    pub citems: Vec<u32>,
}

impl Grid {
    pub fn cell_range(&self, x1: i64, y1: i64, x2: i64, y2: i64) -> (usize, usize, usize, usize) {
        let cx = |x: i64| (((x - self.x0) / self.cw).max(0) as usize).min(self.nx - 1);
        let cy = |y: i64| (((y - self.y0) / self.ch).max(0) as usize).min(self.ny - 1);
        (cx(x1), cy(y1), cx(x2), cy(y2))
    }
}

/// destination of csr_par: each position is handed out once by an atomic counter, so writes never overlap
struct Out(*mut u32);
unsafe impl Sync for Out {}
impl Out {
    fn put(&self, p: usize, v: u32) {
        unsafe { *self.0.add(p) = v }
    }
}

/// lists of items per bucket (`start[c]..start[c + 1]` in `items`), filled on all threads; inside a bucket the
/// items are in ascending order, the same result as filling them one item after another
fn csr_par(buckets: usize, n: usize, each: impl Fn(usize, &mut dyn FnMut(usize)) + Sync) -> (Vec<u32>, Vec<u32>) {
    use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
    let counts: Vec<AtomicU32> = (0..buckets + 1).map(|_| AtomicU32::new(0)).collect();
    (0..n).into_par_iter().for_each(|i| {
        each(i, &mut |c| {
            counts[c + 1].fetch_add(1, Relaxed);
        })
    });
    let mut start: Vec<u32> = counts.into_iter().map(|a| a.into_inner()).collect();
    for k in 1..start.len() {
        start[k] += start[k - 1];
    }
    let fillp: Vec<AtomicU32> = start[..buckets].iter().map(|&v| AtomicU32::new(v)).collect();
    let mut items = vec![0u32; start[buckets] as usize];
    let out = Out(items.as_mut_ptr());
    (0..n).into_par_iter().for_each(|i| {
        each(i, &mut |c| {
            out.put(fillp[c].fetch_add(1, Relaxed) as usize, i as u32);
        })
    });
    drop(fillp);
    // sort every bucket: groups of buckets have contiguous item ranges, one task per group
    let groups = (rayon::current_num_threads() * 8).max(1);
    let mut rest: &mut [u32] = &mut items;
    let mut jobs: Vec<(usize, usize, &mut [u32])> = Vec::with_capacity(groups);
    for g in 0..groups {
        let (c0, c1) = (buckets * g / groups, buckets * (g + 1) / groups);
        let (head, tail) = std::mem::take(&mut rest).split_at_mut((start[c1] - start[c0]) as usize);
        jobs.push((c0, c1, head));
        rest = tail;
    }
    jobs.into_par_iter().for_each(|(c0, c1, sl)| {
        let base = start[c0] as usize;
        for c in c0..c1 {
            sl[start[c] as usize - base..start[c + 1] as usize - base].sort_unstable();
        }
    });
    (start, items)
}

fn build_grid(boxes: &[[i32; 4]], comps: &[[i32; 4]], die: [i32; 4], target: usize) -> Grid {
    let n = boxes.len().max(1);
    let side = ((n as f64 / target as f64).sqrt().ceil() as usize).clamp(16, 2048);
    let (w, h) = ((die[2] - die[0]).max(1) as i64, (die[3] - die[1]).max(1) as i64);
    let (nx, ny) = (side, side);
    let (cw, ch) = ((w + nx as i64 - 1) / nx as i64, (h + ny as i64 - 1) / ny as i64);
    let g0 = Grid { x0: die[0] as i64, y0: die[1] as i64, cw: cw.max(1), ch: ch.max(1), nx, ny, start: vec![], items: vec![], cstart: vec![], citems: vec![] };
    let fill = |list: &[[i32; 4]]| -> (Vec<u32>, Vec<u32>) {
        csr_par(nx * ny, list.len(), |idx, emit| {
            let b = list[idx];
            let (i0, j0, i1, j1) = g0.cell_range(b[0] as i64, b[1] as i64, b[2] as i64, b[3] as i64);
            for j in j0..=j1 {
                for i in i0..=i1 {
                    emit(j * nx + i);
                }
            }
        })
    };
    let ((start, items), (cstart, citems)) = rayon::join(|| fill(boxes), || fill(comps));
    Grid { start, items, cstart, citems, ..g0 }
}

impl Scene {
    pub fn build(lay: &Layout, lib: &Lib, dark: bool) -> Scene {
        Self::build_with(lay, lib, dark, true)
    }

    /// as `build`, but without the GPU instances (20 bytes per shape): `order` (4 bytes per shape) says which
    /// rectangle each position draws, and Gpu::upload_layout makes the instances chunk by chunk
    pub fn build_lean(lay: &Layout, lib: &Lib, dark: bool) -> Scene {
        Self::build_with(lay, lib, dark, false)
    }

    fn build_with(lay: &Layout, lib: &Lib, dark: bool, keep_inst: bool) -> Scene {
        let timing = std::env::var("LSV_DEF_TIMING").is_ok();
        let t0 = std::time::Instant::now();
        let mut styles = layer_styles(lay, lib, dark);
        let nl = styles.len();
        let nslots = 1 + nl * GROUPS;
        let r = &lay.rects;
        let group = group_of;
        // counting sort by slot (component boxes first)
        let mut counts = vec![0u32; nslots + 1];
        counts[1] = lay.comps.len() as u32;
        for k in 0..r.len() {
            if let Some(g) = group(r.kind[k]) {
                counts[slot_of(r.layer[k], g) as usize + 1] += 1;
                styles[r.layer[k] as usize].count += 1;
            }
        }
        for k in 1..counts.len() {
            counts[k] += counts[k - 1];
        }
        let slot_range: Vec<(u32, u32)> = (0..nslots).map(|s| (counts[s], counts[s + 1])).collect();
        let mut fillp = counts.clone();
        let (mut inst, mut order) = (vec![], vec![]);
        if keep_inst {
            inst = vec![Inst::default(); *counts.last().unwrap() as usize];
            for c in &lay.comps {
                inst[fillp[0] as usize] = Inst { r: comp_box(c), slot: SLOT_CELLS };
                fillp[0] += 1;
            }
            for k in 0..r.len() {
                if let Some(g) = group(r.kind[k]) {
                    let s = slot_of(r.layer[k], g) as usize;
                    inst[fillp[s] as usize] = Inst { r: r.r[k], slot: s as u32 };
                    fillp[s] += 1;
                }
            }
        }
        let die = [lay.die[0].round() as i32, lay.die[1].round() as i32, lay.die[2].round() as i32, lay.die[3].round() as i32];
        let total = *counts.last().unwrap() as usize;
        let (mut corder, mut tiles) = (vec![], None);
        if !keep_inst {
            // component boxes take positions [0, comps) (slot 0), rectangles follow in slot order; in large layouts
            // each slot is also sorted by tile so that a view draws only the tiles it shows
            let nc = lay.comps.len();
            // test hook: LSV_LARGE=n treats layouts with n instances or more as large
            let large = std::env::var("LSV_LARGE").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(LARGE);
            let t = if total >= large {
                let (w, h) = ((die[2] - die[0]).max(1) as i64, (die[3] - die[1]).max(1) as i64);
                Some(Tiles { x0: die[0] as i64, y0: die[1] as i64, tw: ((w + TILES as i64 - 1) / TILES as i64).max(1), th: ((h + TILES as i64 - 1) / TILES as i64).max(1), start: vec![] })
            } else {
                None
            };
            let nb = if t.is_some() { BUCKETS } else { 1 };
            let bucket = |b: [i32; 4]| t.as_ref().map(|t| t.bucket(b)).unwrap_or(0);
            // rectangles grouped by (slot, tile), in file order inside a group; component boxes by tile in slot 0
            let (rs, ritems) = csr_par(nslots * nb, r.len(), |k, emit| {
                if let Some(g) = group(r.kind[k]) {
                    emit(slot_of(r.layer[k], g) as usize * nb + bucket(r.r[k]));
                }
            });
            let (cs, citems) = csr_par(nb, nc, |i, emit| emit(bucket(comp_box(&lay.comps[i]))));
            debug_assert_eq!(ritems.len(), total - nc);
            order = ritems;
            corder = citems;
            // bucket starts as instance positions: slot 0 buckets hold the components, the rest follow them
            let bc: Vec<u32> = (0..=nslots * nb).map(|key| if key <= nb { cs[key] } else { nc as u32 + rs[key] }).collect();
            tiles = t.map(|t| Tiles { start: bc, ..t });
        }
        let t1 = std::time::Instant::now();
        let comp_boxes: Vec<[i32; 4]> = lay.comps.iter().map(|c| [c.x as i32, c.y as i32, (c.x + c.w) as i32, (c.y + c.h) as i32]).collect();
        let (grid, (net_start, net_items)) = rayon::join(
            || build_grid(&r.r, &comp_boxes, die, 8),
            || {
                csr_par(lay.nets.len(), r.len(), |k, emit| {
                    let kd = r.kind[k];
                    if (kd == K_WIRE || kd == K_VIA || kd == K_SWIRE || kd == K_SVIA) && r.owner[k] >= 0 {
                        emit(r.owner[k] as usize);
                    }
                })
            },
        );
        let t2 = std::time::Instant::now();
        // pin -> net of every component (for the info panel), in net order as before
        let per: Vec<Vec<(u32, u32)>> = lay
            .nets
            .par_iter()
            .map(|n| n.conns.iter().enumerate().filter(|(_, (c, _))| c != "PIN" && c != "*").filter_map(|(k, (c, _))| lay.comp_index.get(c).map(|&ci| (ci, k as u32))).collect())
            .collect();
        let mut start = vec![0u32; lay.comps.len() + 1];
        for v in &per {
            for &(ci, _) in v {
                start[ci as usize + 1] += 1;
            }
        }
        for k in 1..start.len() {
            start[k] += start[k - 1];
        }
        let mut fillp = start.clone();
        let mut items: Vec<(Arc<str>, u32)> = Vec::with_capacity(*start.last().unwrap() as usize);
        let empty: Arc<str> = Arc::from("");
        items.resize(*start.last().unwrap() as usize, (empty, 0));
        let mut names: HashMap<&str, Arc<str>> = HashMap::new();
        for (ni, v) in per.iter().enumerate() {
            for &(ci, k) in v {
                let p = lay.nets[ni].conns[k as usize].1.as_str();
                let a = match names.get(p) {
                    Some(a) => a.clone(),
                    None => {
                        let a: Arc<str> = Arc::from(p);
                        names.insert(p, a.clone());
                        a
                    }
                };
                items[fillp[ci as usize] as usize] = (a, ni as u32);
                fillp[ci as usize] += 1;
            }
        }
        drop(per);
        let comp_net_pins = CompPins { start, items };
        let t3 = std::time::Instant::now();
        let mut sc = Scene { inst, order, corder, tiles, overview: None, slot_range, styles, die, grid, net_start, net_items, comp_net_pins };
        if sc.tiles.is_some() {
            let (w, h) = ((die[2] - die[0]).max(1) as i64, (die[3] - die[1]).max(1) as i64);
            let (bw, bh) = (((w + OV_BINS as i64 - 1) / OV_BINS as i64).max(1), ((h + OV_BINS as i64 - 1) / OV_BINS as i64).max(1));
            let base = overview_style_base(nslots);
            let nc = lay.comps.len();
            let sref = &sc;
            let per: Vec<Vec<Inst>> = (0..nslots)
                .into_par_iter()
                .map(|s| {
                    let (a, b) = sref.slot_range[s];
                    let mut out = vec![];
                    if a < b {
                        let it = (a as usize..b as usize).map(|i| if i < nc { comp_box(&lay.comps[sref.corder[i] as usize]) } else { r.r[sref.order[i - nc] as usize] });
                        overview_of(it, die[0] as i64, die[1] as i64, bw, bh, base + s as u32, &mut out);
                    }
                    out
                })
                .collect();
            let mut ov = Overview { bin: bw.max(bh) as f64, inst: Vec::with_capacity(per.iter().map(|v| v.len()).sum()), slot_range: vec![] };
            for v in per {
                let a = ov.inst.len() as u32;
                ov.inst.extend(v);
                ov.slot_range.push((a, ov.inst.len() as u32));
            }
            sc.overview = Some(ov);
        }
        if timing {
            eprintln!("scene timing: sort {:.2} s, grid and net lists {:.2} s, component pins {:.2} s, overview {:.2} s ({} rectangles)", (t1 - t0).as_secs_f64(), (t2 - t1).as_secs_f64(), (t3 - t2).as_secs_f64(), t3.elapsed().as_secs_f64(), sc.overview.as_ref().map(|o| o.inst.len()).unwrap_or(0));
        }
        sc
    }

    /// number of GPU instances (component boxes and shapes)
    pub fn inst_count(&self) -> usize {
        self.slot_range.last().map(|s| s.1 as usize).unwrap_or(0)
    }

    /// GPU instances [a, b) of a scene from build_lean, the same values `build` puts in `inst`
    pub fn lean_insts(&self, lay: &Layout, a: usize, b: usize) -> Vec<Inst> {
        let (nc, r) = (lay.comps.len(), &lay.rects);
        (a..b)
            .into_par_iter()
            .map(|i| {
                if i < nc {
                    Inst { r: comp_box(&lay.comps[self.corder[i] as usize]), slot: SLOT_CELLS }
                } else {
                    let k = self.order[i - nc] as usize;
                    Inst { r: r.r[k], slot: slot_of(r.layer[k], group_of(r.kind[k]).unwrap_or(GROUP_SIGNAL)) }
                }
            })
            .collect()
    }

    /// topmost visible shape or component under a world point
    pub fn pick(&self, lay: &Layout, x: i64, y: i64, tol: i64, show: &Show) -> Option<Pick> {
        let g = &self.grid;
        let (i, j, _, _) = g.cell_range(x, y, x, y);
        let c = j * g.nx + i;
        let r = &lay.rects;
        let mut best: Option<(i64, Pick)> = None;
        for &k in &g.items[g.start[c] as usize..g.start[c + 1] as usize] {
            let k = k as usize;
            let b = r.r[k];
            if (x < b[0] as i64 - tol) || (x > b[2] as i64 + tol) || (y < b[1] as i64 - tol) || (y > b[3] as i64 + tol) {
                continue;
            }
            let kd = r.kind[k];
            let grp = match kd {
                K_WIRE | K_VIA => GROUP_SIGNAL,
                K_SWIRE | K_SVIA => GROUP_POWER,
                K_IOPIN => GROUP_PIN,
                _ => continue,
            };
            if !show.groups[grp] || !self.styles[r.layer[k] as usize].visible {
                continue;
            }
            let pri = r.layer[k] as i64 * 2 + if self.styles[r.layer[k] as usize].cut { 0 } else { 1 };
            if best.as_ref().map(|b| pri > b.0).unwrap_or(true) {
                let p = if kd == K_IOPIN { Pick::Pin(r.owner[k] as u32) } else { Pick::Net(r.owner[k] as u32) };
                best = Some((pri, p));
            }
        }
        if let Some((_, p)) = best {
            return Some(p);
        }
        if show.cells {
            for &ci in &g.citems[g.cstart[c] as usize..g.cstart[c + 1] as usize] {
                let cp = &lay.comps[ci as usize];
                if (x as f64) >= cp.x && (x as f64) <= cp.x + cp.w && (y as f64) >= cp.y && (y as f64) <= cp.y + cp.h {
                    return Some(Pick::Comp(ci));
                }
            }
        }
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pick {
    Comp(u32),
    Net(u32),
    Pin(u32),
}

#[derive(Clone, Debug)]
pub struct Show {
    pub cells: bool,
    pub groups: [bool; GROUPS],
}
impl Default for Show {
    fn default() -> Self {
        Show { cells: true, groups: [true; GROUPS] }
    }
}
