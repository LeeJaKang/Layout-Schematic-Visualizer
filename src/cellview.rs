//! Cell tab (port of cellui.js): CDL transistor schematic next to the GDS layout of the same cell,
//! with an LVS-style match so a click on a MOSFET or net highlights its fingers and shapes, and back.
use crate::cdl::{flatten_subckt, parse_cdl, Cdl, Subckt};
use crate::devmatch::{match_cell, MatchResult};
use crate::extract::{extract_cell, parse_layer_map, Extraction, LayerMap, ASAP7_RULES};
use crate::gds::{bbox, flatten, parse_gds, poly_rects, top_cells, Flat, GdsLib};
use crate::schematic::{Canvas, Colors, View};
use crate::xtor::{self, XHit, XLayout};
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, Vec2};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use crate::i18n::tr;
use crate::trf;

/// built-in ASAP7 layer map (moved to pdk.rs)
pub use crate::pdk::ASAP7_LAYERMAP;

const OUTLINE_ONLY: [&str; 9] = ["well", "Nselect", "Pselect", "BOUNDARY", "SLVT", "LVT", "SRAMVT", "SRAMDRC", "TEXT"];
const ORDER: [&str; 18] = ["well", "Nselect", "Pselect", "BOUNDARY", "Active", "fin", "Dummy", "Gate", "GCut", "SDT", "LISD", "LIG", "V0", "M1", "V1", "M2", "V2", "M3"];
const PICK: [&str; 12] = ["M3", "V2", "M2", "V1", "M1", "V0", "LIG", "LISD", "sdc", "gate", "diff", "well"];
const METAL: [u32; 10] = crate::scene::METAL;

fn hexc(h: u32) -> Color32 {
    Color32::from_rgb((h >> 16) as u8, (h >> 8) as u8, h as u8)
}
fn is_cut(base: &str) -> bool {
    let l = base.to_lowercase();
    (l.len() > 1 && l.starts_with('v') && l[1..].chars().all(|c| c.is_ascii_digit())) || l.starts_with("via") || l == "co" || l == "cont" || l == "contact"
}
fn color_for(base: &str, cut: bool, order: usize, dark: bool) -> Color32 {
    if cut {
        return if dark { hexc(0xced4da) } else { hexc(0x343a40) };
    }
    let feol = match base {
        "well" | "nwell" => Some(0x868e96),
        "Nselect" => Some(0x74b816),
        "Pselect" => Some(0xe8590c),
        "Active" => Some(0x40c057),
        "fin" => Some(0xadb5bd),
        "Gate" | "poly" => Some(0xe03131),
        "Dummy" => Some(0xc92a2a),
        "GCut" => Some(0x495057),
        "SDT" => Some(0x82c91e),
        "LISD" => Some(0x9775fa),
        "LIG" => Some(0xf06595),
        "BOUNDARY" => Some(0x868e96),
        _ => None,
    };
    if let Some(h) = feol {
        return hexc(h);
    }
    let lo = base.to_lowercase();
    let mi = ["metal", "met", "m"].iter().find_map(|p| lo.strip_prefix(p).and_then(|d| d.parse::<usize>().ok())).map(|v| v.saturating_sub(1)).unwrap_or(order);
    hexc(METAL[mi % METAL.len()])
}

pub struct GLayer {
    pub key: (i32, i32),
    pub name: String,
    pub base: String,
    pub rects: Vec<[i32; 4]>,
    pub polys: Vec<Vec<i32>>,
    pub color: Color32,
    pub cut: bool,
    pub dir: u8,
    pub outline: bool,
    pub dashed: bool,
    pub z: f32,
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CSel {
    Dev(String),
    Net(String),
    LNet(usize),
    Finger(usize),
}

/// the cell's views (each is a pane of the app's dock)
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CellView {
    Schematic,
    Layout,
    Lef,
}

/// what a cell view asks the app to do with the dock after this frame
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CellAsk {
    Focus(CellView),
    SideBySide,
}

/// cell name -> its output functions as the logic check reads them, or why it has none
type LibFuncs = std::collections::HashMap<String, Result<String, String>>;
/// the Liberty functions read, the files that could not be read as Liberty, and the list of files tried
type LibRead = (LibFuncs, Vec<String>, Vec<PathBuf>);
type LoadResult = Result<(Option<(Cdl, String)>, Option<(GdsLib, String)>, Option<(LayerMap, String)>, Option<(crate::lef::Lib, String)>, Option<(crate::lpe::LpeFile, String)>, Option<LibRead>), String>;

/// a parasitic picked in the Parasitics pane
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PSel {
    Res(usize),
    Cap(usize),
    /// a metal segment (connected piece of one layer of one net)
    Piece(usize),
}

/// colours of the two sides of a coupling capacitor (and of a single R or C)
pub(crate) const SIDE_A: Color32 = Color32::from_rgb(255, 140, 0);
pub(crate) const SIDE_B: Color32 = Color32::from_rgb(0, 180, 255);

/// true when a SPICE file is a post-layout extraction rather than a schematic netlist: .pex
/// includes, a DSPF header, or more R/C element lines than transistors, or R/C lines with coordinates
pub fn looks_like_lpe(text: &str) -> bool {
    let (mut rc, mut m, mut xy) = (0usize, 0usize, false);
    for line in text.lines().take(200_000) {
        let t = line.trim_start();
        let lo = t.to_ascii_lowercase();
        if (lo.starts_with(".include") && (lo.contains(".pex") || lo.contains(".pxi"))) || lo.starts_with("*|dspf") {
            return true;
        }
        match lo.as_bytes().first() {
            Some(b'r') | Some(b'c') => {
                rc += 1;
                if lo.contains("$x=") {
                    xy = true;
                }
            }
            Some(b'm') => m += 1,
            _ => {}
        }
    }
    xy || (rc > 0 && rc > m)
}

/// a steady colour per layer name: metals by their number from the block palette, other layers by a hash
pub fn layer_color(name: &str) -> Color32 {
    let lo = name.to_ascii_lowercase();
    let digits: String = lo.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    let metal = lo.starts_with('m') || lo.starts_with("metal") || lo.starts_with("met");
    let rgb = if metal && !digits.is_empty() && !lo.starts_with("mh") {
        crate::scene::rgb(crate::scene::METAL[digits.parse::<usize>().unwrap_or(1).saturating_sub(1) % crate::scene::METAL.len()])
    } else if lo == "unknown" || lo.is_empty() {
        [0.55, 0.55, 0.55]
    } else {
        let h = lo.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
        let hue = (h % 360) as f32 / 360.0;
        let c = egui::ecolor::Hsva::new(hue, 0.55, 0.85, 1.0).to_rgb();
        [c[0], c[1], c[2]]
    };
    Color32::from_rgb((rgb[0] * 255.0) as u8, (rgb[1] * 255.0) as u8, (rgb[2] * 255.0) as u8)
}

pub(crate) fn fmt_ohm(v: f64) -> String {
    if v >= 1000.0 {
        format!("{:.3} kΩ", v / 1000.0)
    } else {
        format!("{:.2} Ω", v)
    }
}

/// element name without the extractor's per-net subcircuit prefix (x_PM_CELL%NET/r38 -> r38)
fn short(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

pub(crate) fn fmt_f(v: f64) -> String {
    let ff = v * 1e15;
    if ff.abs() >= 0.01 {
        format!("{:.4} fF", ff)
    } else {
        format!("{:.3} aF", ff * 1000.0)
    }
}

/// a line drawn twice, wide and white under narrow `col`, so it stays visible over any layer colour
fn outlined(p: &egui::Painter, pts: Vec<Pos2>, w: f32, col: Color32) {
    p.add(egui::Shape::line(pts.clone(), Stroke::new(w + 2.5, Color32::WHITE)));
    p.add(egui::Shape::line(pts, Stroke::new(w, col)));
}

/// a resistor symbol from `a` to `b`: straight leads and a zigzag in the middle (at least 28 px long, along
/// x when the two points coincide)
pub(crate) fn draw_resistor(p: &egui::Painter, a: Pos2, b: Pos2, col: Color32) {
    let d = b - a;
    let len = d.length();
    let (a, b) = if len < 28.0 {
        let dir = if len < 1e-3 { Vec2::X } else { d / len };
        let m = a + d * 0.5;
        (m - dir * 14.0, m + dir * 14.0)
    } else {
        (a, b)
    };
    let d = b - a;
    let len = d.length();
    let dir = d / len;
    let n = Vec2::new(-dir.y, dir.x);
    let zl = len.clamp(20.0, 40.0);
    let z0 = a + dir * ((len - zl) / 2.0);
    let mut pts = vec![a, z0];
    for i in 0..6 {
        let t = (i as f32 + 0.5) / 6.0;
        pts.push(z0 + dir * (zl * t) + n * if i % 2 == 0 { 5.0 } else { -5.0 });
    }
    pts.push(z0 + dir * zl);
    pts.push(b);
    outlined(p, pts, 2.0, col);
}

/// a capacitor symbol from `a` to `b`: leads and two plates across the line at its middle (at least 30 px
/// long, along x when the two points coincide)
pub(crate) fn draw_capacitor(p: &egui::Painter, a: Pos2, b: Pos2, col: Color32) {
    let d = b - a;
    let len = d.length();
    let dir = if len < 1e-3 { Vec2::X } else { d / len };
    let m = a + d * 0.5;
    let (a2, b2) = if len < 30.0 { (m - dir * 15.0, m + dir * 15.0) } else { (a, b) };
    let n = Vec2::new(-dir.y, dir.x);
    let g = 3.5;
    outlined(p, vec![a2, m - dir * g], 1.5, col);
    outlined(p, vec![m + dir * g, b2], 1.5, col);
    for c in [m - dir * g, m + dir * g] {
        outlined(p, vec![c - n * 9.0, c + n * 9.0], 2.5, col);
    }
}

/// a ground symbol at `top`: three bars, narrowing away from the capacitor (downward, or upward when `up`)
pub(crate) fn draw_ground(p: &egui::Painter, top: Pos2, up: bool, col: Color32) {
    let s = if up { -1.0 } else { 1.0 };
    for (i, w) in [11.0, 7.0, 3.0].iter().enumerate() {
        let y = top.y + s * 4.0 * i as f32;
        outlined(p, vec![Pos2::new(top.x - w, y), Pos2::new(top.x + w, y)], 2.0, col);
    }
}

#[derive(Default, Clone)]
pub struct CellFiles {
    pub cdl: Option<PathBuf>,
    pub gds: Option<PathBuf>,
    pub layermap: Option<PathBuf>,
    pub lef: Option<PathBuf>,
    pub lpe: Option<PathBuf>,
    pub cell: Option<String>,
    /// Liberty files: the functions the logic check starts from
    pub libs: Vec<PathBuf>,
}

pub struct CellTab {
    pub files: CellFiles,
    rx: Option<Receiver<LoadResult>>,
    pub cdl: Option<Cdl>,
    pub cdl_name: String,
    pub gds: Option<GdsLib>,
    pub gds_name: String,
    pub lmap: LayerMap,
    pub lmap_name: String,
    pub rules: String,
    pub rules_edit: String,
    pub names: Vec<String>,
    pub cell: Option<String>,
    pub sc: Option<Subckt>,
    pub flat: Option<Flat>,
    pub ex: Option<Extraction>,
    pub m: Option<MatchResult>,
    pub xl: Option<XLayout>,
    pub layers: Vec<GLayer>,
    pub sel: Option<CSel>,
    hover: Option<CSel>,
    xview: View,
    xfit: bool,
    xcenter: Option<Pos2>,
    // GDS view: screen = (world - origin) * scale, y flipped
    gorigin: [f64; 2],
    gscale: f64,
    gfit: bool,
    pub status: String,
    pub filter: String,
    want_cell: Option<String>,
    // cell LEF (abstract view)
    pub lef: Option<crate::lef::Lib>,
    pub lef_name: String,
    // post-layout netlist: cells by name, layers of the current cell's parasitics, the picked one
    pub lpe: std::collections::HashMap<String, crate::lpe::LpeCell>,
    pub lpe_name: String,
    pub lpe_new: bool,
    // notes of reading the post-layout file (includes, lines not read)
    lpe_warn: Vec<String>,
    attr: Option<crate::lpe::Attribution>,
    seg: Option<crate::lpe::Segmentation>,
    piece_res: Vec<Vec<usize>>,
    piece_caps: Vec<Vec<(usize, usize)>>,
    pview: u8,
    pempty: bool,
    mpd: f64,
    pub psel: Option<PSel>,
    pfilter: String,
    pkinds: [bool; 3],
    pmark: bool,
    lorigin: [f64; 2],
    lscale: f64,
    lfit: bool,
    // dock state the app shares with the views: requests out, side-by-side state in
    pub ask: Vec<CellAsk>,
    pub side_by_side: bool,
    // logic check: functions from the Liberty files read, the text per cell, the last result
    pub lib_funcs: LibFuncs,
    libs_read: Vec<PathBuf>,
    lib_notes: Vec<String>,
    pub func_text: std::collections::HashMap<String, String>,
    pub func_out: Option<Result<crate::func::Outcome, String>>,
    func_only: bool,
}


impl CellTab {
    pub fn new(files: CellFiles) -> CellTab {
        let mut t = CellTab {
            files,
            rx: None,
            cdl: None,
            cdl_name: String::new(),
            gds: None,
            gds_name: String::new(),
            lmap: parse_layer_map(ASAP7_LAYERMAP),
            lmap_name: tr("ASAP7 (built in)").into(),
            rules: ASAP7_RULES.to_string(),
            rules_edit: ASAP7_RULES.to_string(),
            names: vec![],
            cell: None,
            sc: None,
            flat: None,
            ex: None,
            m: None,
            xl: None,
            layers: vec![],
            sel: None,
            hover: None,
            xview: View { pan: Vec2::ZERO, zoom: 1.0 },
            xfit: true,
            xcenter: None,
            gorigin: [0.0, 0.0],
            gscale: 1.0,
            gfit: true,
            status: String::new(),
            filter: String::new(),
            want_cell: None,
            lef: None,
            lef_name: String::new(),
            lpe: std::collections::HashMap::new(),
            lpe_name: String::new(),
            lpe_new: false,
            lpe_warn: vec![],
            attr: None,
            seg: None,
            piece_res: vec![],
            piece_caps: vec![],
            pview: 0,
            pempty: false,
            mpd: 1e-9,
            psel: None,
            pfilter: String::new(),
            pkinds: [true, true, true],
            pmark: false,
            lorigin: [0.0, 0.0],
            lscale: 1.0,
            lfit: true,
            ask: vec![],
            side_by_side: false,
            lib_funcs: LibFuncs::new(),
            libs_read: vec![],
            lib_notes: vec![],
            func_text: std::collections::HashMap::new(),
            func_out: None,
            func_only: false,
        };
        t.want_cell = t.files.cell.clone();
        t
    }

    /// Liberty files added (or changed) since the last read, with nothing being read now
    pub fn libs_pending(&self) -> bool {
        self.files.libs != self.libs_read && !self.loading()
    }

    pub fn has_input(&self) -> bool {
        self.files.cdl.is_some() || self.files.gds.is_some() || self.files.lef.is_some() || self.files.lpe.is_some()
    }
    pub fn loading(&self) -> bool {
        self.rx.is_some()
    }
    pub fn has_cell(&self, name: &str) -> bool {
        self.cdl.as_ref().map_or(false, |c| c.subckts.contains_key(name)) || self.gds.as_ref().map_or(false, |g| g.cells.contains_key(name))
    }

    /// read the files given (only the ones that changed) in the background
    pub fn start_load(&mut self, ctx: &egui::Context, cdl: bool, gds: bool, lmap: bool, lef: bool, lpe: bool) {
        let (tx, rx) = channel();
        let f = self.files.clone();
        let ctx = ctx.clone();
        // the Liberty files only when the list changed
        // the Liberty files when the list differs from the one last read, and with a full reload (an edited file)
        let libs = f.libs != self.libs_read || (cdl && gds && !f.libs.is_empty());
        std::thread::spawn(move || {
            let r = (|| -> LoadResult {
                let rd = |p: &PathBuf| crate::io::read_all(p).map_err(|e| format!("{}: {}", p.display(), e));
                let c = match (&f.cdl, cdl) {
                    (Some(p), true) => Some((parse_cdl(&String::from_utf8_lossy(&rd(p)?), &p.display().to_string()), p.file_name().unwrap_or_default().to_string_lossy().to_string())),
                    _ => None,
                };
                let g = match (&f.gds, gds) {
                    (Some(p), true) => Some((parse_gds(&rd(p)?, &p.display().to_string())?, p.file_name().unwrap_or_default().to_string_lossy().to_string())),
                    _ => None,
                };
                let l = match (&f.layermap, lmap) {
                    (Some(p), true) => Some((parse_layer_map(&String::from_utf8_lossy(&rd(p)?)), p.file_name().unwrap_or_default().to_string_lossy().to_string())),
                    _ => None,
                };
                let lf = match (&f.lef, lef) {
                    (Some(p), true) => {
                        let mut lib = crate::lef::Lib::default();
                        crate::lef::parse_lef(&rd(p)?, &p.display().to_string(), &mut lib)?;
                        Some((lib, p.file_name().unwrap_or_default().to_string_lossy().to_string()))
                    }
                    _ => None,
                };
                let lp = match (&f.lpe, lpe) {
                    (Some(p), true) => Some((crate::lpe::read_lpe(p)?, p.file_name().unwrap_or_default().to_string_lossy().to_string())),
                    _ => None,
                };
                // a file that is not Liberty (a SPICE model library named .lib) is noted, it does not stop the cells
                let lb = if libs {
                    let mut m = LibFuncs::new();
                    let mut notes = vec![];
                    for p in &f.libs {
                        match rd(p).and_then(|b| crate::liberty::parse_liberty(&b, &p.display().to_string())) {
                            Ok(cells) => {
                                for c in cells {
                                    m.entry(c.name.clone()).or_insert_with(|| crate::func::liberty_text(&c));
                                }
                            }
                            Err(e) => notes.push(e),
                        }
                    }
                    Some((m, notes, f.libs.clone()))
                } else {
                    None
                };
                Ok((c, g, l, lf, lp, lb))
            })();
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        self.rx = Some(rx);
        self.status = tr("Reading the CDL / GDS…").into();
    }

    /// returns Some(status) when a load finished
    pub fn poll(&mut self) -> Option<String> {
        let r = self.rx.as_ref()?.try_recv().ok()?;
        self.rx = None;
        match r {
            Err(e) => {
                self.status = trf!("Error: {}", e);
            }
            Ok((c, g, l, lf, lp, lb)) => {
                if let Some((m, notes, read)) = lb {
                    // texts left empty or still the old Liberty's follow the new one, edited texts stay
                    let old = std::mem::replace(&mut self.lib_funcs, m);
                    self.func_text.retain(|c, t| !(t.trim().is_empty() || matches!(old.get(c), Some(Ok(o)) if o == t)));
                    self.libs_read = read;
                    self.lib_notes = notes;
                }
                if let Some((file, n)) = lp {
                    self.lpe.clear();
                    self.lpe_warn = file.warnings.clone();
                    for cell in file.cells {
                        self.lpe.insert(cell.name.clone(), cell);
                    }
                    self.lpe_name = n;
                    self.lpe_new = true;
                    if let Some(first) = self.lpe.keys().next().cloned() {
                        if self.want_cell.is_none() {
                            self.want_cell = Some(first);
                        }
                    }
                }
                if let Some((lib, n)) = lf {
                    self.lef = Some(lib);
                    self.lef_name = n;
                    self.lfit = true;
                }
                if let Some((c, n)) = c {
                    self.cdl = Some(c);
                    self.cdl_name = n;
                }
                if let Some((g, n)) = g {
                    self.gds = Some(g);
                    self.gds_name = n;
                }
                if let Some((l, n)) = l {
                    self.lmap = l;
                    self.lmap_name = n;
                }
                // cells of the post-layout netlist take the place of their CDL subcircuits (schematic and LVS)
                if !self.lpe.is_empty() {
                    if self.cdl.is_none() {
                        self.cdl = Some(Cdl { subckts: std::collections::HashMap::new(), order: vec![], warnings: vec![] });
                        self.cdl_name = self.lpe_name.clone();
                    }
                    if let Some(c) = self.cdl.as_mut() {
                        for (name, cell) in &self.lpe {
                            if !c.subckts.contains_key(name) {
                                c.order.push(name.clone());
                            }
                            c.subckts.insert(name.clone(), cell.subckt.clone());
                        }
                    }
                }
                self.auto_preset();
                self.fill_names();
                let want = self.want_cell.take().or_else(|| self.cell.clone()).filter(|n| self.names.contains(n));
                let pick = want.or_else(|| self.names.iter().find(|n| self.cdl.as_ref().map_or(false, |c| c.subckts.contains_key(*n)) && self.gds.as_ref().map_or(false, |g| g.cells.contains_key(*n))).cloned()).or_else(|| self.names.first().cloned());
                if let Some(p) = pick {
                    self.show_cell(&p);
                }
                let mut parts = vec![];
                if let Some(c) = &self.cdl {
                    parts.push(trf!("CDL {}: {} subcircuits{}", self.cdl_name, c.subckts.len(), if c.warnings.is_empty() { String::new() } else { trf!(", {} warnings", c.warnings.len()) }));
                }
                if let Some(g) = &self.gds {
                    parts.push(trf!("GDS {}: {} cells", self.gds_name, g.cells.len()));
                }
                if let Some(l) = &self.lef {
                    parts.push(trf!("LEF {}: {} macros", self.lef_name, l.macros.len()));
                }
                parts.push(trf!("layer map {}", self.lmap_name));
                if !crate::i18n::made_from(&self.status, "Error: {}") || self.cell.is_some() {
                    let s = parts.join(" | ");
                    self.status = format!("{}{}", s, if self.status.is_empty() || crate::i18n::made_from(&self.status, "Reading the CDL / GDS…") { String::new() } else { format!(" | {}", self.status) });
                }
                let notes = std::mem::take(&mut self.lib_notes);
                if !notes.is_empty() {
                    self.status = format!("{} | {}", self.status, trf!("Liberty: {}", notes.join(" | ")));
                }
            }
        }
        Some(self.status.clone())
    }

    /// with no layer map file given and the rules still an unedited preset, take the layer map and rules
    /// of the preset detected from the GDS layers (built in, or a *.lsvpdk file in LSV_PDK_PATH)
    fn auto_preset(&mut self) {
        let g = match &self.gds {
            Some(g) if self.files.layermap.is_none() => g,
            _ => return,
        };
        let (list, _) = crate::pdk::all_presets();
        if !list.iter().any(|p| p.rules == self.rules) {
            return;
        }
        if let Some(k) = crate::pdk::detect_among(&list, &crate::pdk::layers_used(g)) {
            let p = list[k];
            self.lmap = parse_layer_map(p.layermap);
            // all_presets lists the built-in presets first, then the *.lsvpdk files of LSV_PDK_PATH
            let origin = if k < crate::pdk::presets().len() { tr("built in") } else { tr("LSV_PDK_PATH file") };
            self.lmap_name = trf!("{} ({}, detected)", p.name, origin);
            self.rules = p.rules.to_string();
            self.rules_edit = self.rules.clone();
        }
    }

    fn fill_names(&mut self) {
        let mut set: HashSet<String> = HashSet::new();
        if let Some(c) = &self.cdl {
            for (n, s) in &c.subckts {
                if !s.mos.is_empty() || !s.insts.is_empty() {
                    set.insert(n.clone());
                }
            }
        }
        if let Some(g) = &self.gds {
            for n in top_cells(g) {
                set.insert(n);
            }
        }
        if let Some(l) = &self.lef {
            for n in l.macros.keys() {
                set.insert(n.clone());
            }
        }
        let mut v: Vec<String> = set.into_iter().collect();
        v.sort();
        self.names = v;
    }

    pub fn request_cell(&mut self, name: &str) {
        if self.loading() {
            self.want_cell = Some(name.to_string());
        } else if self.has_cell(name) {
            self.show_cell(name);
        } else {
            self.status = trf!("{} is not in the loaded CDL or GDS", name);
        }
    }

    pub fn show_cell(&mut self, name: &str) {
        let t0 = std::time::Instant::now();
        self.cell = Some(name.to_string());
        self.sel = None;
        self.hover = None;
        self.sc = self.cdl.as_ref().filter(|c| c.subckts.contains_key(name)).and_then(|c| flatten_subckt(c, name).ok());
        self.flat = None;
        self.ex = None;
        self.m = None;
        self.xl = None;
        let mut errs = vec![];
        if let Some(g) = self.gds.as_ref().filter(|g| g.cells.contains_key(name)) {
            match flatten(g, name) {
                Ok(f) => self.flat = Some(f),
                Err(e) => errs.push(e),
            }
        }
        if let Some(f) = &self.flat {
            match extract_cell(f, &self.lmap, &self.rules) {
                Ok(ex) => self.ex = Some(ex),
                Err(e) => errs.push(e),
            }
        }
        if let (Some(ex), Some(sc)) = (&self.ex, &self.sc) {
            if ex.errors.is_empty() {
                self.m = Some(match_cell(ex, sc, true));
            }
        }
        // layers of the post-layout parasitics of this cell
        self.attr = None;
        self.psel = None;
        self.mpd = self.gds.as_ref().map_or(1e-9, |g| g.meters_per_db);
        if let (Some(lc), Some(ex), Some(m), Some(f)) = (self.lpe.get(name), &self.ex, &self.m, &self.flat) {
            self.attr = Some(crate::lpe::attribute_layers(lc, ex, m, f, self.mpd, &self.rules));
            // test hook: LSV_PSEL=<element name> picks that R or C of the post-layout netlist
            if let Ok(want) = std::env::var("LSV_PSEL") {
                self.psel = lc.caps.iter().position(|c| c.name == want || short(&c.name) == want).map(PSel::Cap).or_else(|| lc.res.iter().position(|r| short(&r.name) == want || r.name == want).map(PSel::Res));
            }
        }
        self.build_segments();
        // test hook: LSV_PVIEW=segments opens the parasitics list by segment
        if std::env::var("LSV_PVIEW").map_or(false, |v| v == "segments") {
            self.pview = 1;
        }
        // test hook: LSV_PFILTER=<text> fills the parasitics filter
        if let Ok(f) = std::env::var("LSV_PFILTER") {
            self.pfilter = f;
        }
        if let Some(sc) = &self.sc {
            self.xl = Some(xtor::layout(sc));
        }
        self.build_layers();
        self.xfit = true;
        self.gfit = true;
        self.lfit = true;
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        self.status = if !errs.is_empty() {
            trf!("Error: {}", errs.join("; "))
        } else if self.sc.is_none() {
            trf!("{}: in the GDS but not in the CDL, so there is no schematic to match", name)
        } else if self.flat.is_none() && self.gds.is_some() {
            trf!("{}: in the CDL but not in the GDS", name)
        } else {
            trf!("{}: {} transistors | {} | {:.0} ms", name, self.sc.as_ref().map_or(0, |s| s.mos.len()), self.lvs_text(), ms)
        };
        // logic check: the text kept for this cell, else the Liberty functions
        if self.func_text.get(name).map_or(true, |t| t.trim().is_empty()) {
            if let Some(Ok(t)) = self.lib_funcs.get(name) {
                self.func_text.insert(name.to_string(), t.clone());
            }
        }
        // test hook: LSV_FUNC=<functions> fills the logic check of the cell shown
        if let Ok(f) = std::env::var("LSV_FUNC") {
            self.func_text.insert(name.to_string(), f);
        }
        self.run_logic();
        // test hook: LSV_CSEL=<transistor or net of the schematic> picks it, in the schematic and the layout
        if let Ok(s) = std::env::var("LSV_CSEL") {
            let sc = self.sc.as_ref();
            let pick = if sc.map_or(false, |c| c.mos.iter().any(|d| d.name == s)) {
                Some(CSel::Dev(s))
            } else if sc.map_or(false, |c| c.mos.iter().any(|d| d.d == s || d.g == s || d.s == s)) {
                Some(CSel::Net(s))
            } else {
                None
            };
            if pick.is_some() {
                self.select(pick, false);
            }
        }
        // symmetric pairing and likely mismatch causes, appended to the status line
        if let Some(m) = &self.m {
            if m.ok && !m.symmetric_devs.is_empty() {
                self.status.push_str(&trf!(" | {} transistors in symmetric copies, paired by order", m.symmetric_devs.len()));
            }
            if !m.ok && !m.causes.is_empty() {
                self.status.push_str(&trf!(" | likely cause: {}", m.causes.join(" | ")));
            }
        }
    }

    fn build_layers(&mut self) {
        let prev: std::collections::HashMap<(i32, i32), bool> = self.layers.iter().map(|l| (l.key, l.visible)).collect();
        self.layers.clear();
        let f = match &self.flat {
            Some(f) => f,
            None => return,
        };
        let mut keys: Vec<(i32, i32)> = vec![];
        let mut by: std::collections::HashMap<(i32, i32), Vec<usize>> = std::collections::HashMap::new();
        for (i, p) in f.polys.iter().enumerate() {
            let k = (p.layer as i32, p.dt as i32);
            if !by.contains_key(&k) {
                keys.push(k);
            }
            by.entry(k).or_default().push(i);
        }
        let mut order = 0usize;
        for k in keys {
            let name = self.lmap.by_num.get(&k).cloned().unwrap_or_else(|| format!("{}/{}", k.0, k.1));
            let base = name.split('.').next().unwrap_or("").to_string();
            let cut = is_cut(&base);
            let color = color_for(&base, cut, order, false);
            order += 1;
            let z = ORDER.iter().position(|x| *x == base).map(|v| v as f32).unwrap_or(50.0 + k.0 as f32 / 1000.0);
            let dir = match base.strip_prefix('M').and_then(|d| d.parse::<u32>().ok()) {
                Some(n) => if n % 2 == 1 { 1 } else { 2 },
                None => if base == "Gate" { 2 } else { 1 },
            };
            let polys: Vec<Vec<i32>> = by[&k].iter().map(|&i| f.polys[i].pts.clone()).collect();
            let rects: Vec<[i32; 4]> = polys.iter().flat_map(|p| poly_rects(p)).collect();
            self.layers.push(GLayer { key: k, outline: OUTLINE_ONLY.contains(&base.as_str()), dashed: base == "BOUNDARY" || base == "well", visible: prev.get(&k).copied().unwrap_or(base != "fin"), name, base, rects, polys, color, cut, dir, z });
        }
        self.layers.sort_by(|a, b| a.z.partial_cmp(&b.z).unwrap());
    }

    pub fn lvs_text(&self) -> String {
        let sc = match &self.sc {
            Some(s) => s,
            None => return if self.flat.is_some() { tr("no CDL subcircuit for this cell").into() } else { String::new() },
        };
        if self.flat.is_none() {
            return tr("no GDS cell for this subcircuit").into();
        }
        if let Some(ex) = &self.ex {
            if !ex.errors.is_empty() {
                return trf!("extraction rules: {} errors", ex.errors.len());
            }
        }
        let nets: HashSet<&String> = sc.mos.iter().flat_map(|d| [&d.d, &d.g, &d.s]).collect();
        match &self.m {
            Some(m) if m.ok => trf!("layout matches the CDL: {} transistors, {} nets", sc.mos.len(), nets.len()),
            Some(m) => trf!("layout does NOT match the CDL: {} of {} device groups matched", m.matched_groups, m.cdl_groups),
            None => String::new(),
        }
    }

    // ---------- selection helpers ----------
    fn net_rects(&self, ids: &[usize]) -> (Vec<[i32; 4]>, Vec<String>) {
        let mut out = vec![];
        let mut used = vec![];
        if let Some(ex) = &self.ex {
            for (layer, by) in &ex.shapes {
                for id in ids {
                    if let Some(r) = by.get(id) {
                        if !r.is_empty() {
                            out.extend_from_slice(r);
                            if !used.contains(layer) {
                                used.push(layer.clone());
                            }
                        }
                    }
                }
            }
        }
        (out, used)
    }
    fn layout_nets_of(&self, name: &str) -> Vec<usize> {
        self.m.as_ref().and_then(|m| m.net_to_layout.get(name).cloned()).unwrap_or_default()
    }
    /// (net rects, device channel rects, device source/drain active rects) to highlight in the GDS
    fn highlight(&self) -> (Vec<[i32; 4]>, Vec<[i32; 4]>, Vec<[i32; 4]>) {
        let ex = match &self.ex {
            Some(e) => e,
            None => return (vec![], vec![], vec![]),
        };
        let fingers = |f: &[usize]| -> (Vec<[i32; 4]>, Vec<[i32; 4]>) { (f.iter().flat_map(|&i| ex.devices[i].rects.clone()).collect(), f.iter().flat_map(|&i| ex.devices[i].sd_rects.clone()).collect()) };
        match &self.sel {
            Some(CSel::Dev(n)) => {
                let f = self.m.as_ref().and_then(|m| m.dev_to_layout.get(n).cloned()).unwrap_or_default();
                let (ch, act) = fingers(&f);
                (vec![], ch, act)
            }
            Some(CSel::Net(n)) => (self.net_rects(&self.layout_nets_of(n)).0, vec![], vec![]),
            Some(CSel::LNet(id)) => (self.net_rects(&[*id]).0, vec![], vec![]),
            Some(CSel::Finger(i)) => {
                let (ch, act) = fingers(&[*i]);
                (vec![], ch, act)
            }
            None => (vec![], vec![], vec![]),
        }
    }
    fn xsel(&self) -> Option<XHit> {
        match &self.sel {
            Some(CSel::Dev(n)) => Some(XHit::Dev(n.clone())),
            Some(CSel::Net(n)) => Some(XHit::Net(n.clone())),
            _ => None,
        }
    }
    fn from_layout(&self, h: Option<CSel>) -> Option<CSel> {
        let m = self.m.as_ref();
        match h? {
            CSel::Finger(i) => match m.and_then(|m| m.layout_to_dev.get(&i)).and_then(|v| v.first()) {
                Some(n) => Some(CSel::Dev(n.clone())),
                None => Some(CSel::Finger(i)),
            },
            CSel::LNet(id) => match m.and_then(|m| m.net_map.get(&id).or_else(|| m.layout_to_net.get(&id))) {
                Some(c) => Some(CSel::Net(c.clone())),
                None => Some(CSel::LNet(id)),
            },
            other => Some(other),
        }
    }
    fn label(&self, s: &CSel) -> String {
        match s {
            CSel::Dev(n) => match self.sc.as_ref().and_then(|sc| sc.mos.iter().find(|d| d.name == *n)) {
                Some(d) => format!("{}  ·  {} {}", n, d.typ, d.g),
                None => n.clone(),
            },
            CSel::Net(n) => n.clone(),
            CSel::Finger(i) => self.ex.as_ref().map(|e| e.devices[*i].typ.clone()).unwrap_or_default(),
            CSel::LNet(id) => self.ex.as_ref().and_then(|e| e.nets[*id].name.clone()).unwrap_or_else(|| format!("#{}", id)),
        }
    }
    fn source_visible(&self, name: &str) -> bool {
        let mut nm = name.to_string();
        let rules = crate::extract::parse_rules(&self.rules);
        for _ in 0..8 {
            match rules.layers.iter().find(|l| l.0 == nm) {
                Some(d) => nm = d.1.first().cloned().unwrap_or_default(),
                None => break,
            }
        }
        self.layers.iter().find(|l| l.base == nm).map_or(true, |l| l.visible)
    }
    fn gpick(&self, x: f64, y: f64, tol: f64) -> Option<CSel> {
        let ex = self.ex.as_ref()?;
        let inside = |r: &[i32; 4]| x >= r[0] as f64 - tol && x <= r[2] as f64 + tol && y >= r[1] as f64 - tol && y <= r[3] as f64 + tol;
        if self.source_visible("gate") {
            for (i, d) in ex.devices.iter().enumerate() {
                if d.rects.iter().any(inside) {
                    return Some(CSel::Finger(i));
                }
            }
        }
        let mut order: Vec<&String> = PICK.iter().filter_map(|n| ex.shapes.iter().find(|s| s.0 == *n).map(|s| &s.0)).collect();
        for s in &ex.shapes {
            if !PICK.contains(&s.0.as_str()) {
                order.push(&s.0);
            }
        }
        for nm in order {
            if !self.source_visible(nm) {
                continue;
            }
            let by = &ex.shapes.iter().find(|s| s.0 == *nm).unwrap().1;
            let mut ids: Vec<&usize> = by.keys().collect();
            ids.sort();
            for id in ids {
                if by[id].iter().any(inside) {
                    return Some(CSel::LNet(*id));
                }
            }
        }
        None
    }

    // ---------- search ----------
    pub fn search(&self, q: &str) -> Vec<(u8, String)> {
        let q = q.trim().to_lowercase();
        if q.is_empty() {
            return vec![];
        }
        let mut out: Vec<(u8, String)> = self.names.iter().filter(|n| n.to_lowercase().contains(&q)).map(|n| (0u8, n.clone())).collect();
        if let Some(sc) = &self.sc {
            for d in &sc.mos {
                if d.name.to_lowercase().contains(&q) {
                    out.push((1, d.name.clone()));
                }
            }
            let mut nets: Vec<&String> = sc.ports.iter().chain(sc.mos.iter().flat_map(|d| [&d.d, &d.g, &d.s])).collect();
            nets.sort();
            nets.dedup();
            for n in nets {
                if n.to_lowercase().contains(&q) {
                    out.push((2, n.clone()));
                }
            }
        }
        out.sort_by(|a, b| a.1.len().cmp(&b.1.len()));
        out
    }
    pub fn go(&mut self, r: &(u8, String)) {
        match r.0 {
            0 => self.show_cell(&r.1.clone()),
            1 => self.select(Some(CSel::Dev(r.1.clone())), true),
            _ => self.select(Some(CSel::Net(r.1.clone())), true),
        }
    }
    fn select(&mut self, s: Option<CSel>, center: bool) {
        self.sel = s;
        if center {
            if let (Some(xl), Some(sc), Some(h)) = (&self.xl, &self.sc, self.xsel()) {
                if let Some(p) = xtor::locate(xl, sc, &h) {
                    self.xcenter = Some(p);
                }
            }
        }
    }

    // ---------- side panel ----------
    pub fn side_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading(tr("Cell"));
        if self.names.is_empty() {
            ui.weak(if self.loading() { tr("Reading…") } else { tr("Open a CDL netlist and/or a GDS file (CDL… / GDS… above).") });
            return;
        }
        let mut pick: Option<String> = None;
        let cur = self.cell.clone().unwrap_or_default();
        egui::ComboBox::from_id_salt("cellpick").selected_text(&cur).width(260.0).height(400.0).show_ui(ui, |ui| {
            ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(tr("filter")));
            let f = self.filter.to_lowercase();
            for n in self.names.iter().filter(|n| f.is_empty() || n.to_lowercase().contains(&f)) {
                let in_c = self.cdl.as_ref().map_or(false, |c| c.subckts.contains_key(n));
                let in_g = self.gds.as_ref().map_or(false, |g| g.cells.contains_key(n));
                let tag = if self.cdl.is_some() && self.gds.is_some() { if in_c && in_g { "" } else if in_c { "  (CDL)" } else { "  (GDS)" } } else { "" };
                if ui.selectable_label(*n == cur, format!("{}{}", n, tag)).clicked() {
                    pick = Some(n.clone());
                }
            }
        });
        if let Some(p) = pick {
            self.show_cell(&p);
        }
        let mut sbs = self.side_by_side;
        if ui.checkbox(&mut sbs, tr("Schematic and layout side by side")).changed() {
            self.ask.push(CellAsk::SideBySide);
        }
        let lvs = self.lvs_text();
        if !lvs.is_empty() {
            let ok = self.m.as_ref().map_or(false, |m| m.ok);
            let c = if ok { Color32::from_rgb(47, 158, 68) } else if self.m.is_some() || crate::i18n::made_from(&lvs, "extraction rules: {} errors") { Color32::from_rgb(201, 42, 42) } else { ui.visuals().weak_text_color() };
            ui.colored_label(c, lvs);
            if let Some(m) = &self.m {
                if m.ok && !m.symmetric_devs.is_empty() {
                    ui.small(trf!("Symmetric: {} transistors have interchangeable layout copies (nets {}). Every pairing of the copies passes. The one shown pairs CDL order with layout left to right.", m.symmetric_devs.len(), m.symmetric_nets.join(" ")));
                }
                for cs in &m.causes {
                    ui.colored_label(Color32::from_rgb(201, 42, 42), trf!("cause: {}", cs));
                }
            }
        }
        ui.small(trf!("layer map: {}", self.lmap_name));
        ui.separator();
        egui::CollapsingHeader::new(egui::RichText::new(tr("Logic check")).strong()).id_salt("logic").default_open(true).show(ui, |ui| self.logic_ui(ui));
        ui.separator();
        ui.heading(tr("Selection"));
        let mut next: Option<CSel> = None;
        match self.sel.clone() {
            None => {
                if let Some(sc) = &self.sc {
                    ui.label(trf!("{} ports · {} transistors", sc.ports.len(), sc.mos.len()));
                    ui.horizontal_wrapped(|ui| {
                        for p in &sc.ports {
                            if ui.link(egui::RichText::new(p).monospace()).clicked() {
                                next = Some(CSel::Net(p.clone()));
                            }
                        }
                    });
                }
                if let Some(ex) = &self.ex {
                    if !ex.errors.is_empty() {
                        ui.strong(tr("Extraction rule errors"));
                        for e in ex.errors.iter().take(12) {
                            ui.monospace(e);
                        }
                    }
                    if !ex.warnings.is_empty() {
                        ui.strong(tr("Extraction warnings"));
                        for w in ex.warnings.iter().take(12) {
                            ui.monospace(w);
                        }
                    }
                }
                if let Some(m) = self.m.as_ref().filter(|m| !m.ok) {
                    if !m.unmatched_cdl.is_empty() {
                        ui.strong(tr("CDL devices without a layout match"));
                        for u in &m.unmatched_cdl {
                            ui.monospace(u);
                        }
                    }
                    for c in &m.conflicts {
                        ui.monospace(c);
                    }
                }
                if self.sc.is_some() || self.ex.is_some() {
                    ui.weak(tr("Click a transistor or a net, in the schematic or in the layout."));
                }
            }
            Some(CSel::Dev(n)) => {
                if let Some(d) = self.sc.as_ref().and_then(|sc| sc.mos.iter().find(|d| d.name == n)).cloned() {
                    ui.strong(&d.name);
                    ui.label(format!("{} · {}", d.typ, d.model));
                    if self.m.as_ref().map_or(false, |m| m.symmetric_devs.contains(&d.name)) {
                        ui.small(tr("symmetric copy: another layout copy matches as well, this one is paired by order"));
                    }
                    let num = |v: f64| if v.is_finite() { format!("{}", (v * 1e9 * 1000.0).round() / 1000.0) } else { "-".into() };
                    ui.monospace(format!("w {} nm   l {} nm   nfin {}", num(d.w), num(d.l), if d.nfin.is_finite() { format!("{}", d.nfin) } else { "-".into() }));
                    for (k, net) in [("D", &d.d), ("G", &d.g), ("S", &d.s), ("B", &d.b)] {
                        ui.horizontal(|ui| {
                            ui.monospace(k);
                            if ui.link(net).clicked() {
                                next = Some(CSel::Net(net.clone()));
                            }
                        });
                    }
                    if let Some(ex) = &self.ex {
                        let f = self.m.as_ref().and_then(|m| m.dev_to_layout.get(&d.name).cloned()).unwrap_or_default();
                        let fins: f64 = f.iter().map(|&i| ex.devices[i].nfin).sum();
                        if f.is_empty() {
                            ui.label(tr("no layout fingers matched"));
                        } else {
                            ui.label(trf!("layout: {} finger{}, {} fins (highlighted)", f.len(), if f.len() == 1 { "" } else { "s" }, fins));
                        }
                    }
                }
            }
            Some(CSel::Net(n)) => {
                ui.strong(&n);
                if let Some(sc) = &self.sc {
                    for d in sc.mos.iter().filter(|d| d.d == n || d.g == n || d.s == n) {
                        let roles: String = [("D", &d.d), ("G", &d.g), ("S", &d.s)].iter().filter(|(_, x)| **x == n).map(|(k, _)| *k).collect();
                        ui.horizontal(|ui| {
                            if ui.link(&d.name).clicked() {
                                next = Some(CSel::Dev(d.name.clone()));
                            }
                            ui.monospace(roles);
                        });
                    }
                }
                if self.ex.is_some() {
                    let (r, layers) = self.net_rects(&self.layout_nets_of(&n));
                    if !r.is_empty() {
                        ui.label(trf!("layout: {} shapes on {} (highlighted)", r.len(), layers.join(", ")));
                    } else {
                        ui.label(tr("no layout shapes for this net (an internal node inside merged diffusion, or no match)"));
                    }
                }
            }
            Some(CSel::LNet(id)) => {
                if let Some(ex) = &self.ex {
                    let nt = &ex.nets[id];
                    ui.strong(nt.name.clone().unwrap_or_else(|| format!("#{}", id)));
                    ui.label(trf!("layout net on {}", nt.layers.iter().filter(|x| !x.starts_with('@')).cloned().collect::<Vec<_>>().join(", ")));
                    ui.weak(tr("layout only: no CDL net matched"));
                }
            }
            Some(CSel::Finger(i)) => {
                if let Some(ex) = &self.ex {
                    ui.strong(&ex.devices[i].typ);
                    ui.label(format!("nfin {}", ex.devices[i].nfin));
                    ui.weak(tr("layout only: no CDL device matched"));
                }
            }
        }
        if let Some(n) = next {
            self.select(Some(n), true);
        }
        if !self.layers.is_empty() {
            ui.separator();
            ui.heading(tr("GDS layers"));
            for l in self.layers.iter_mut() {
                ui.horizontal(|ui| {
                    let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                    ui.painter().rect_filled(r, 2.0, l.color);
                    ui.checkbox(&mut l.visible, format!("{}  {}/{}", l.name, l.key.0, l.key.1));
                });
            }
        }
        ui.separator();
        egui::CollapsingHeader::new(tr("Extraction rules")).id_salt("rules").default_open(false).show(ui, |ui| {
            ui.add(egui::TextEdit::multiline(&mut self.rules_edit).code_editor().desired_rows(14).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                if ui.button(tr("Apply")).clicked() {
                    self.rules = self.rules_edit.clone();
                    if let Some(c) = self.cell.clone() {
                        self.show_cell(&c);
                    }
                }
                if ui.button(tr("ASAP7 defaults")).clicked() {
                    self.rules = ASAP7_RULES.to_string();
                    self.rules_edit = self.rules.clone();
                    if let Some(c) = self.cell.clone() {
                        self.show_cell(&c);
                    }
                }
            });
        });
    }

    /// check the transistors of the cell (schematic or post-layout netlist, and the layout) against the functions
    pub fn run_logic(&mut self) {
        let text = self.cell.as_ref().and_then(|c| self.func_text.get(c)).cloned().unwrap_or_default();
        if text.trim().is_empty() || (self.sc.is_none() && self.ex.is_none()) {
            self.func_out = None;
            return;
        }
        let sch = self.sc.as_ref().map(crate::func::Circuit::from_subckt);
        let lay = self.ex.as_ref().filter(|e| e.errors.is_empty()).map(|e| crate::func::Circuit::from_extraction(e, self.m.as_ref()));
        let post = self.cell.as_ref().map_or(false, |c| self.lpe.contains_key(c));
        let mut cs: Vec<(String, &crate::func::Circuit)> = vec![];
        if let Some(c) = &sch {
            cs.push(((if post { tr("post-layout netlist") } else { tr("schematic") }).to_string(), c));
        }
        if let Some(c) = &lay {
            cs.push((tr("layout").to_string(), c));
        }
        let mut out = crate::func::run(&text, &cs);
        // a layout whose extraction has rule errors is not checked, and says so
        if let (Ok(o), Some(e)) = (&mut out, self.ex.as_ref().filter(|e| !e.errors.is_empty())) {
            o.got.push((tr("layout").to_string(), Err(trf!("extraction rules: {} errors", e.errors.len()))));
        }
        self.func_out = Some(out);
    }

    fn logic_ui(&mut self, ui: &mut egui::Ui) {
        let Some(cell) = self.cell.clone() else { return };
        if self.sc.is_none() && self.ex.is_none() {
            ui.weak(tr("Needs the transistors of the cell: a CDL or post-layout netlist, or a GDS."));
            return;
        }
        let (green, red) = (Color32::from_rgb(47, 158, 68), Color32::from_rgb(201, 42, 42));
        ui.small(tr("The function of each output as in Liberty, one per line, for example Y = !(A & B). Every input vector is simulated on the transistors."));
        let lib = self.lib_funcs.get(&cell).cloned();
        // an edit is kept per cell; an untouched empty box stores nothing, so a Liberty read later still fills it
        let mut text = self.func_text.get(&cell).cloned().unwrap_or_default();
        let r = ui.add(egui::TextEdit::multiline(&mut text).code_editor().desired_rows(2).desired_width(f32::INFINITY).hint_text("Y = !(A & B)"));
        if r.changed() {
            self.func_text.insert(cell.clone(), text);
        }
        let mut run = r.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.command);
        ui.horizontal_wrapped(|ui| {
            if ui.button(tr("Check")).clicked() {
                run = true;
            }
            match &lib {
                Some(Ok(t)) => {
                    if ui.button(tr("From Liberty")).on_hover_text(t.as_str()).clicked() {
                        self.func_text.insert(cell.clone(), t.clone());
                        run = true;
                    }
                }
                Some(Err(why)) => {
                    ui.weak(trf!("Liberty: {}", why));
                }
                None => {}
            }
            ui.checkbox(&mut self.func_only, tr("only differing vectors"));
        });
        if run {
            self.run_logic();
        }
        let o = match &self.func_out {
            None => return,
            Some(Err(e)) => {
                ui.colored_label(red, e);
                return;
            }
            Some(Ok(o)) => o,
        };
        for (k, (label, g)) in o.got.iter().enumerate() {
            match (g, o.differ(k)) {
                (Err(e), _) => ui.colored_label(red, format!("{}: {}", label, e)),
                (Ok(_), Some(0)) => ui.colored_label(green, trf!("{}: all {} input vectors match", label, o.vectors())),
                (Ok(_), d) => ui.colored_label(red, trf!("{}: {} of {} input vectors differ", label, d.unwrap_or(0), o.vectors())),
            };
        }
        if !o.unused.is_empty() {
            ui.small(trf!("Not in the functions, left floating: {}", o.unused.join(" ")));
        }
        // the truth table: inputs, then per output the function, and a column for each circuit that differs on
        // that output (where all circuits match, their levels are the function's), a differing level in red
        const MAX_ROWS: usize = 256;
        let differs = |v: usize| o.got.iter().any(|(_, g)| g.as_ref().map_or(false, |g| g[v] != o.want[v]));
        let all: Vec<usize> = (0..o.vectors()).filter(|&v| !self.func_only || differs(v)).collect();
        let sims: Vec<(&String, &Vec<Vec<u8>>)> = o.got.iter().filter_map(|(l, g)| g.as_ref().ok().map(|g| (l, g))).collect();
        let cols: Vec<Vec<usize>> = (0..o.outputs.len()).map(|j| (0..sims.len()).filter(|&k| (0..o.vectors()).any(|v| sims[k].1[v][j] != o.want[v][j])).collect()).collect();
        egui::ScrollArea::both().id_salt("logic_rows").max_height(260.0).auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("logic_grid").striped(true).spacing([12.0, 2.0]).show(ui, |ui| {
                for n in &o.inputs {
                    ui.monospace(n);
                }
                for (j, out) in o.outputs.iter().enumerate() {
                    ui.label(egui::RichText::new(out).monospace().strong());
                    for &k in &cols[j] {
                        ui.label(egui::RichText::new(format!("{} · {}", out, sims[k].0)).monospace().color(red));
                    }
                }
                ui.end_row();
                for &v in all.iter().take(MAX_ROWS) {
                    for k in 0..o.inputs.len() {
                        ui.monospace(format!("{}", crate::func::input_bit(v, k, o.inputs.len())));
                    }
                    for (j, _) in o.outputs.iter().enumerate() {
                        let w = o.want[v][j];
                        ui.label(egui::RichText::new(crate::func::level_char(w).to_string()).monospace().strong());
                        for &k in &cols[j] {
                            let x = sims[k].1[v][j];
                            let t = egui::RichText::new(crate::func::level_char(x).to_string()).monospace();
                            ui.label(if x == w { t } else { t.color(red).strong() });
                        }
                    }
                    ui.end_row();
                }
            });
        });
        if all.len() > MAX_ROWS {
            ui.weak(trf!("{} of {} rows shown", MAX_ROWS, all.len()));
        }
    }

    pub fn fit(&mut self) {
        self.xfit = true;
        self.gfit = true;
        self.lfit = true;
    }

    /// schematic pane: the transistor schematic of the current cell
    pub fn schematic_pane(&mut self, ui: &mut egui::Ui, rect: Rect, ctx: &egui::Context, col: &Colors) {
        if self.sc.is_none() || self.xl.is_none() {
            self.pane_hint(ui, rect, "schematic", "open a CDL or LPE SPICE netlist");
            return;
        }
        self.hover = None;
        self.xtor_canvas(ui, rect, ctx, col);
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.sel = None;
        }
    }

    /// layout pane: the GDS of the current cell
    pub fn layout_pane(&mut self, ui: &mut egui::Ui, rect: Rect, ctx: &egui::Context, col: &Colors) {
        if self.flat.is_none() {
            self.pane_hint(ui, rect, "layout", "open a GDS file");
            return;
        }
        self.gds_canvas(ui, rect, ctx, col);
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.sel = None;
        }
    }

    fn pane_hint(&self, ui: &mut egui::Ui, rect: Rect, what: &str, how: &str) {
        let msg = if self.loading() {
            tr("Reading…").to_string()
        } else {
            match &self.cell {
                Some(c) => trf!("{} has no {}: {}", c, tr(what), tr(how)),
                None => tr("Drop a CDL netlist and a GDS file here").to_string(),
            }
        };
        ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(16.0), ui.visuals().weak_text_color());
    }

    /// the LEF library that holds the current cell: the cell LEF, else the block's LEF
    pub fn lef_lib<'a>(&'a self, block: Option<&'a crate::lef::Lib>) -> Option<&'a crate::lef::Lib> {
        let cell = self.cell.as_deref()?;
        self.lef.as_ref().filter(|l| l.macros.contains_key(cell)).or(block.filter(|l| l.macros.contains_key(cell)))
    }

    /// LEF pane: the abstract of the current cell (size, pins by layer, obstructions)
    pub fn lef_pane(&mut self, ui: &mut egui::Ui, rect: Rect, ctx: &egui::Context, col: &Colors, block: Option<&crate::lef::Lib>) {
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let cell = self.cell.clone().unwrap_or_default();
        let Some(lib) = self.lef_lib(block) else {
            let how = if self.lef.is_none() && block.is_none() { "open a cell LEF".to_string() } else { "it is not in the LEF".to_string() };
            self.pane_hint(ui, rect, "LEF abstract", &how);
            return;
        };
        let m = lib.macros[&cell].clone();
        let lib_layers = lib.layers.clone();
        let lib_order = lib.layer_order.clone();
        let vias = lib.vias.clone();
        if self.lfit {
            let s = ((rect.width() as f64 - 40.0) / m.w.max(1e-6)).min((rect.height() as f64 - 40.0) / m.h.max(1e-6)).max(1e-3);
            self.lscale = s;
            self.lorigin = [m.w / 2.0, m.h / 2.0];
            self.lfit = false;
        }
        let c = rect.center();
        if resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            self.lorigin[0] -= d.x as f64 / self.lscale;
            self.lorigin[1] += d.y as f64 / self.lscale;
        }
        let to_world = |o: [f64; 2], s: f64, p: Pos2| [o[0] + (p.x - c.x) as f64 / s, o[1] - (p.y - c.y) as f64 / s];
        let mut hover_pin: Option<usize> = None;
        // pin and obstruction rectangles in microns, vias expanded
        let expand = |shapes: &[crate::lef::MacroShape]| -> Vec<crate::lef::Shape> {
            let mut out = vec![];
            for s in shapes {
                match s {
                    crate::lef::MacroShape::Rect(r) => out.push(r.clone()),
                    crate::lef::MacroShape::Via { name, at } => {
                        if let Some(v) = vias.get(name) {
                            for vs in &v.shapes {
                                out.push(crate::lef::Shape { layer: vs.layer.clone(), r: [vs.r[0] + at[0], vs.r[1] + at[1], vs.r[2] + at[0], vs.r[3] + at[1]] });
                            }
                        }
                    }
                }
            }
            out
        };
        let pins: Vec<(String, String, Vec<crate::lef::Shape>)> = m.pins.iter().map(|p| (p.name.clone(), p.dir.clone(), expand(&p.shapes))).collect();
        let obs = expand(&m.obs);
        if let Some(pos) = resp.hover_pos() {
            let (zf, pan) = crate::nav::wheel(ctx);
            if zf != 1.0 {
                let before = to_world(self.lorigin, self.lscale, pos);
                self.lscale *= zf as f64;
                let after = to_world(self.lorigin, self.lscale, pos);
                self.lorigin[0] += before[0] - after[0];
                self.lorigin[1] += before[1] - after[1];
            }
            self.lorigin[0] -= pan.x as f64 / self.lscale;
            self.lorigin[1] += pan.y as f64 / self.lscale;
            let w = to_world(self.lorigin, self.lscale, pos);
            hover_pin = pins.iter().position(|(_, _, sh)| sh.iter().any(|s| w[0] >= s.r[0] && w[0] <= s.r[2] && w[1] >= s.r[1] && w[1] <= s.r[3]));
            if resp.clicked() || (resp.secondary_clicked() && hover_pin.is_some()) {
                let s = hover_pin.map(|i| CSel::Net(self.port_name(&pins[i].0)));
                self.select(s, true);
            }
        }
        let (o, s) = (self.lorigin, self.lscale);
        let sx = |x: f64| c.x + ((x - o[0]) * s) as f32;
        let sy = |y: f64| c.y - ((y - o[1]) * s) as f32;
        let srect = |r: &[f64; 4]| Rect::from_min_max(Pos2::new(sx(r[0]), sy(r[3])), Pos2::new(sx(r[2]), sy(r[1])));
        let painter = ui.painter_at(rect);
        let fg = ui.visuals().text_color();
        let weak = ui.visuals().weak_text_color();
        painter.rect_stroke(srect(&[0.0, 0.0, m.w, m.h]), 0.0, Stroke::new(1.0, weak), egui::StrokeKind::Middle);
        let lay_color = |name: &str| -> Color32 {
            let kind = lib_layers.get(name).map(|l| l.kind.as_str()).unwrap_or("");
            if kind == "CUT" {
                return Color32::from_gray(150);
            }
            let digits: String = name.chars().skip_while(|ch| !ch.is_ascii_digit()).take_while(|ch| ch.is_ascii_digit()).collect();
            let idx = digits.parse::<usize>().ok().map(|v| v.saturating_sub(1)).unwrap_or_else(|| lib_order.iter().filter(|l| lib_layers.get(*l).map_or(false, |x| x.kind == "ROUTING")).position(|l| l == name).unwrap_or(0));
            let c = crate::scene::rgb(crate::scene::METAL[idx % crate::scene::METAL.len()]);
            Color32::from_rgb((c[0] * 255.0) as u8, (c[1] * 255.0) as u8, (c[2] * 255.0) as u8)
        };
        for sh in &obs {
            let r = srect(&sh.r);
            painter.rect_filled(r, 0.0, Color32::from_rgba_unmultiplied(128, 128, 128, 40));
            painter.rect_stroke(r, 0.0, Stroke::new(1.0, Color32::from_gray(120)), egui::StrokeKind::Middle);
        }
        let sel_net = match &self.sel {
            Some(CSel::Net(n)) => Some(n.to_ascii_lowercase()),
            _ => None,
        };
        for (i, (name, _, shapes)) in pins.iter().enumerate() {
            let on = sel_net.as_deref() == Some(name.to_ascii_lowercase().as_str()) || hover_pin == Some(i);
            for sh in shapes {
                let lc = lay_color(&sh.layer);
                let r = srect(&sh.r);
                let fill = if on { col.hl.gamma_multiply(0.55) } else { lc.gamma_multiply(0.35) };
                painter.rect_filled(r, 0.0, fill);
                painter.rect_stroke(r, 0.0, Stroke::new(if on { 2.0 } else { 1.0 }, if on { col.hl } else { lc }), egui::StrokeKind::Middle);
            }
            if let Some(big) = shapes.iter().max_by(|a, b| ((a.r[2] - a.r[0]) * (a.r[3] - a.r[1])).partial_cmp(&((b.r[2] - b.r[0]) * (b.r[3] - b.r[1]))).unwrap_or(std::cmp::Ordering::Equal)) {
                painter.text(srect(&big.r).center(), egui::Align2::CENTER_CENTER, name, egui::FontId::proportional(13.0), fg);
            }
        }
        painter.text(rect.left_top() + Vec2::new(8.0, 8.0), egui::Align2::LEFT_TOP, trf!("{}  ·  {} × {} µm  ·  {} pins  ·  {} obstruction shapes", cell, m.w, m.h, pins.len(), obs.len()), egui::FontId::proportional(12.0), weak);
        let has_x = self.sc.is_some();
        let has_g = self.flat.is_some();
        let mut asks = vec![];
        resp.context_menu(|ui| {
            if ui.add_enabled(has_x, egui::Button::new(tr("Show in schematic"))).clicked() {
                asks.push(CellAsk::Focus(CellView::Schematic));
                ui.close();
            }
            if ui.add_enabled(has_g, egui::Button::new(tr("Show in layout"))).clicked() {
                asks.push(CellAsk::Focus(CellView::Layout));
                ui.close();
            }
        });
        self.ask.extend(asks);
        if let Some(i) = hover_pin {
            let (name, dir, shapes) = &pins[i];
            let layers: Vec<&str> = shapes.iter().map(|s| s.layer.as_str()).collect::<HashSet<_>>().into_iter().collect();
            resp.on_hover_text_at_pointer(trf!("pin {} ({})  ·  {}", name, if dir.is_empty() { "-" } else { dir }, layers.join(", ")));
        }
    }

    // ---------- post-layout parasitics ----------
    /// the current cell has a post-layout netlist
    pub fn has_lpe_cell(&self) -> bool {
        self.cell.as_ref().map_or(false, |c| self.lpe.contains_key(c))
    }

    fn um_to_db(&self, v: f64) -> f64 {
        v * 1e-6 / self.mpd
    }

    /// split the current cell's parasitics into metal segments (called after the layers are known)
    fn build_segments(&mut self) {
        self.seg = None;
        self.piece_res = vec![];
        self.piece_caps = vec![];
        let (Some(name), Some(at), Some(ex), Some(m)) = (self.cell.as_ref(), self.attr.as_ref(), self.ex.as_ref(), self.m.as_ref()) else { return };
        let Some(lc) = self.lpe.get(name) else { return };
        let seg = crate::lpe::segment_parasitics(lc, at, ex, m, self.mpd);
        let mut pr = vec![vec![]; seg.segs.len()];
        let mut pc = vec![vec![]; seg.segs.len()];
        for (k, r) in seg.res.iter().enumerate() {
            if let Ok(i) = r {
                pr[*i].push(k);
            }
        }
        for (k, s) in seg.caps.iter().enumerate() {
            for (side, cs) in s.iter().enumerate() {
                if let Some(cs) = cs {
                    pc[cs.seg].push((k, side));
                }
            }
        }
        self.piece_res = pr;
        self.piece_caps = pc;
        self.seg = Some(seg);
    }

    fn seg_label(&self, i: usize) -> String {
        match (self.cell.as_ref().and_then(|c| self.lpe.get(c)), self.seg.as_ref()) {
            (Some(lc), Some(seg)) if i < seg.segs.len() => crate::lpe::seg_label(lc, seg, i),
            _ => "?".into(),
        }
    }

    /// where a resistor sits, in words: its segment, and for a wire the length between its two nodes
    fn res_seg_text(&self, k: usize) -> String {
        let Some(seg) = self.seg.as_ref() else { return "-".into() };
        match seg.res.get(k) {
            Some(Ok(i)) if seg.segs[*i].kind == crate::lpe::SegKind::Wire => {
                let (len, _) = crate::lpe::seg_size_um(&seg.segs[*i], self.mpd);
                format!("{} ({:.3} µm)", self.seg_label(*i), len)
            }
            Some(Ok(i)) => self.seg_label(*i),
            _ => "-".into(),
        }
    }

    fn cap_seg_text(&self, k: usize) -> String {
        let Some(s) = self.seg.as_ref().and_then(|s| s.caps.get(k)) else { return "-".into() };
        let l = |c: &Option<crate::lpe::CapSide>| c.as_ref().map(|c| self.seg_label(c.seg)).unwrap_or_else(|| "-".into());
        if self.cell.as_ref().and_then(|c| self.lpe.get(c)).and_then(|lc| lc.caps.get(k)).map_or(false, |c| c.coupling) {
            format!("{} / {}", l(&s[0]), l(&s[1]))
        } else {
            l(&s[0])
        }
    }

    /// draw the picked parasitic over the cell layout: its metal segment lit, a resistor along the wire of an R
    /// between its two nodes, a capacitor down to a ground symbol for a C to ground, a capacitor between the two
    /// segments of a coupling C (and, when asked, a dot on every capacitor point)
    fn paint_parasitics(&self, painter: &egui::Painter, rect: Rect) {
        let (Some(name), Some(attr), Some(seg)) = (self.cell.as_ref(), self.attr.as_ref(), self.seg.as_ref()) else { return };
        let Some(lc) = self.lpe.get(name) else { return };
        let c = rect.center();
        let (o, s) = (self.gorigin, self.gscale);
        let sp = |x: f64, y: f64| Pos2::new(c.x + ((x - o[0]) * s) as f32, c.y - ((y - o[1]) * s) as f32);
        let srect = |r: &[i32; 4]| Rect::from_min_max(sp(r[0] as f64, r[3] as f64), sp(r[2] as f64, r[1] as f64));
        let um = |p: [f64; 2]| sp(self.um_to_db(p[0]), self.um_to_db(p[1]));
        let db = |p: [f64; 2]| sp(p[0], p[1]);
        if self.pmark {
            for (i, cap) in lc.caps.iter().enumerate() {
                let layer = attr.caps.get(i).and_then(|a| a.a.clone());
                if let Some(p) = cap.xy {
                    painter.circle_filled(um(p), 2.5, layer_color(layer.as_deref().unwrap_or("")));
                }
            }
        }
        let rects = |rs: &[[i32; 4]], col: Color32| {
            for r in rs {
                painter.rect_filled(srect(r), 0.0, col.gamma_multiply(0.45));
                painter.rect_stroke(srect(r), 0.0, Stroke::new(2.0, col), egui::StrokeKind::Middle);
            }
        };
        let dot = |p: Pos2, col: Color32| {
            painter.circle_filled(p, 4.0, col);
            painter.circle_stroke(p, 4.0, Stroke::new(1.5, Color32::BLACK));
        };
        let font = egui::FontId::proportional(12.0);
        let label = |at: Pos2, text: String, col: Color32| {
            let g = painter.layout_no_wrap(text, font.clone(), Color32::BLACK);
            let r = Rect::from_min_size(at, g.size()).expand(2.0);
            painter.rect_filled(r, 3.0, Color32::from_white_alpha(235));
            painter.rect_stroke(r, 3.0, Stroke::new(1.0, col), egui::StrokeKind::Outside);
            painter.galley(at, g, Color32::BLACK);
        };
        // one R: its segment, and the resistor between its two node points inside it
        let res = |k: usize| {
            let Some(Ok(i)) = seg.res.get(k) else { return };
            let sg = &seg.segs[*i];
            rects(&sg.rects, SIDE_A);
            let (a, b) = (db(sg.ends[0]), db(sg.ends[1]));
            draw_resistor(painter, a, b, Color32::BLACK);
            dot(a, SIDE_A);
            dot(b, SIDE_A);
            let m = a + (b - a) * 0.5;
            label(m + Vec2::new(10.0, 10.0), format!("{} {}", short(&lc.res[k].name), fmt_ohm(lc.res[k].ohms)), SIDE_A);
        };
        match self.psel {
            Some(PSel::Res(k)) => res(k),
            Some(PSel::Cap(k)) => {
                let (Some(cap), Some(sides)) = (lc.caps.get(k), seg.caps.get(k)) else { return };
                if let Some(a) = &sides[0] {
                    rects(&seg.segs[a.seg].rects, SIDE_A);
                }
                if let Some(b) = &sides[1] {
                    rects(&seg.segs[b.seg].rects, SIDE_B);
                }
                let text = format!("{} {}", short(&cap.name), fmt_f(cap.farads));
                match (&sides[0], &sides[1]) {
                    (Some(a), Some(b)) if cap.coupling => {
                        let (pa, pb) = (db(a.at), db(b.at));
                        draw_capacitor(painter, pa, pb, Color32::BLACK);
                        dot(pa, SIDE_A);
                        dot(pb, SIDE_B);
                        label(pa + (pb - pa) * 0.5 + Vec2::new(12.0, 10.0), text, SIDE_B);
                    }
                    (Some(a), _) => {
                        // down to ground, or up when the point is in the lower half of the view
                        let pa = db(a.at);
                        let up = pa.y > c.y;
                        let g = pa + Vec2::new(0.0, if up { -34.0 } else { 34.0 });
                        draw_capacitor(painter, pa, g, Color32::BLACK);
                        draw_ground(painter, g, up, Color32::BLACK);
                        dot(pa, SIDE_A);
                        label(pa + Vec2::new(14.0, if up { -24.0 } else { 10.0 }), text, SIDE_A);
                    }
                    _ => {}
                }
            }
            Some(PSel::Piece(p)) => {
                let Some(sg) = seg.segs.get(p) else { return };
                match sg.res {
                    Some(k) => res(k),
                    None => rects(&sg.rects, SIDE_A),
                }
                for &(k, side) in self.piece_caps.get(p).into_iter().flatten() {
                    if let Some(cs) = &seg.caps[k][side] {
                        dot(db(cs.at), if lc.caps[k].coupling { SIDE_B } else { SIDE_A });
                    }
                }
            }
            None => {}
        }
    }

    /// Parasitics pane: every R, C and coupling C of the cell's post-layout netlist, by element or by metal segment
    pub fn parasitics_pane(&mut self, ui: &mut egui::Ui) {
        let Some(name) = self.cell.clone() else {
            ui.weak(tr("No cell yet."));
            return;
        };
        let Some(lc) = self.lpe.get(&name) else {
            if self.lpe.is_empty() {
                ui.weak(tr("Open an LPE (post-layout) SPICE netlist with File > Cell > Open CDL / SPICE / LPE netlist… to list its resistors and capacitors here."));
            } else {
                let mut cells: Vec<&String> = self.lpe.keys().collect();
                cells.sort();
                ui.weak(trf!("{} is not in {}. Cells there: {}", name, self.lpe_name, cells.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
            }
            return;
        };
        let attr = self.attr.as_ref();
        let ncc = lc.caps.iter().filter(|c| c.coupling).count();
        ui.horizontal_wrapped(|ui| {
            ui.strong(&name);
            ui.label(trf!("{} R · {} C · {} coupling C · {}", lc.res.len(), lc.caps.len() - ncc, ncc, self.lpe_name));
        });
        // what reading the file and placing its parasitics noted: file and cell warnings, DSPF facts, layer notes
        let lnotes: Vec<String> = self.lpe_warn.iter().map(|w| format!("file: {}", w)).chain(lc.warnings.iter().map(|w| format!("warning: {}", w))).chain(lc.notes.iter().cloned()).chain(attr.map(|a| a.notes.clone()).unwrap_or_default()).collect();
        if !lnotes.is_empty() {
            egui::CollapsingHeader::new(trf!("Reading notes ({})", lnotes.len())).id_salt("preadnotes").default_open(false).show(ui, |ui| {
                for w in &lnotes {
                    ui.small(w);
                }
            });
        }
        if attr.is_none() {
            ui.colored_label(Color32::from_rgb(201, 42, 42), tr("No layers: the GDS of this cell is needed (and its LVS must match) to place the parasitics on layers and segments."));
        }
        // totals per layer
        if let Some(at) = attr {
            let mut by: std::collections::BTreeMap<String, (usize, f64, f64, f64)> = std::collections::BTreeMap::new();
            for (i, r) in lc.res.iter().enumerate() {
                let l = at.res.get(i).and_then(|x| x.layer.clone()).unwrap_or_else(|| "unknown".into());
                let e = by.entry(l).or_default();
                e.0 += 1;
                e.1 += r.ohms;
            }
            for (i, c) in lc.caps.iter().enumerate() {
                let cl = at.caps.get(i);
                if c.coupling {
                    for l in [cl.and_then(|x| x.a.clone()), cl.and_then(|x| x.b.clone())] {
                        by.entry(l.unwrap_or_else(|| "unknown".into())).or_default().3 += c.farads * 0.5;
                    }
                } else {
                    by.entry(cl.and_then(|x| x.a.clone()).unwrap_or_else(|| "unknown".into())).or_default().2 += c.farads;
                }
            }
            egui::CollapsingHeader::new(tr("Totals by layer")).id_salt("ptotals_h").default_open(false).show(ui, |ui| {
                egui::Grid::new("ptotals").num_columns(5).spacing([14.0, 2.0]).striped(true).show(ui, |ui| {
                    for h in ["layer", "R", "ΣR", "ΣC to ground", "ΣCc (half per side)"] {
                        ui.label(egui::RichText::new(tr(h)).small().weak());
                    }
                    ui.end_row();
                    for (l, (n, sr, sc, scc)) in &by {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                            ui.painter().rect_filled(r, 2.0, layer_color(l));
                            ui.monospace(if l == "unknown" { tr("unknown") } else { l.as_str() });
                        });
                        ui.monospace(n.to_string());
                        ui.monospace(fmt_ohm(*sr));
                        ui.monospace(fmt_f(*sc));
                        ui.monospace(fmt_f(*scc));
                        ui.end_row();
                    }
                });
            });
            let stack = at.nodes.iter().filter(|n| n.src == crate::lpe::Src::Stack).count();
            if stack > 0 {
                ui.small(trf!("{} nodes sit on a via stack with no way to tell the layer from the netlist: they take the top layer of the stack (marked * below).", stack));
            }
            let by_value = at.res.iter().filter(|r| r.by_value).count();
            if by_value > 0 {
                ui.small(trf!("{} R take the layer of R placed by position in this cell whose value they equal, not their own position (marked = below).", by_value));
            }
        }
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut self.pview, 0, tr("By element"));
            ui.selectable_value(&mut self.pview, 1, tr("By segment"));
            ui.separator();
            if self.pview == 0 {
                ui.checkbox(&mut self.pkinds[0], "R");
                ui.checkbox(&mut self.pkinds[1], "C");
                ui.checkbox(&mut self.pkinds[2], tr("Coupling C"));
            } else {
                ui.checkbox(&mut self.pempty, tr("Segments with no R or C"));
            }
            ui.add(egui::TextEdit::singleline(&mut self.pfilter).hint_text(tr("net, layer or segment")).desired_width(140.0));
            ui.checkbox(&mut self.pmark, tr("Dots on the layout"));
        });
        if self.pview == 1 {
            ui.small(tr("A segment is the metal of one R between its two nodes (wire), the via of one R between layers, or the metal under a capacitor that no wire covers (patch). Each capacitor side sits on the segment under its point."));
        }
        ui.horizontal(|ui| {
            for (col, txt) in [(SIDE_A, "side A"), (SIDE_B, "side B of a coupling C")] {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), egui::Sense::hover());
                ui.painter().rect_filled(r, 2.0, col);
                ui.small(tr(txt));
            }
        });
        let f = self.pfilter.to_lowercase();
        let netn = |i: usize| lc.nets.get(i).map(|n| n.name.as_str()).unwrap_or("?");
        // rows: what a click picks, the value to sort by, the cells (drawn at fixed columns, so a translated word does not shift the rest)
        let mut rows: Vec<(PSel, f64, Vec<String>)> = vec![];
        if self.pview == 0 {
            if self.pkinds[0] {
                for (i, r) in lc.res.iter().enumerate() {
                    let rl = attr.and_then(|a| a.res.get(i));
                    // a layer the netlist leaves open but the segment found from the end positions
                    let by_pos = self.seg.as_ref().and_then(|s| match s.res.get(i) {
                        Some(Ok(k)) => Some(s.segs[*k].layer.clone()),
                        _ => None,
                    });
                    let lay = rl.and_then(|x| x.layer.clone()).or_else(|| by_pos.map(|l| trf!("{} (by position)", l))).unwrap_or_else(|| tr("unknown").into());
                    let star = rl.map_or(false, |x| x.ends_src.contains(&crate::lpe::Src::Stack));
                    let mark = if rl.map_or(false, |x| x.by_value) { " =" } else if star { " *" } else { "" };
                    rows.push((PSel::Res(i), r.ohms, vec![format!("R   {:<8} {:>11}", short(&r.name), fmt_ohm(r.ohms)), netn(r.net).to_string(), self.res_seg_text(i), format!("{}{}", lay, mark)]));
                }
            }
            for (i, c) in lc.caps.iter().enumerate() {
                if (c.coupling && !self.pkinds[2]) || (!c.coupling && !self.pkinds[1]) {
                    continue;
                }
                let cl = attr.and_then(|a| a.caps.get(i));
                let la = cl.and_then(|x| x.a.clone()).unwrap_or_else(|| tr("unknown").into());
                let t = if c.coupling {
                    let lb = cl.and_then(|x| x.b.clone()).unwrap_or_else(|| tr("unknown").into());
                    vec![format!("Cc  {:<8} {:>11}", short(&c.name), fmt_f(c.farads)), format!("{} / {}", netn(c.net_a), c.net_b.map(netn).unwrap_or("?")), self.cap_seg_text(i), format!("{} / {}", la, lb)]
                } else {
                    vec![format!("C   {:<8} {:>11}", short(&c.name), fmt_f(c.farads)), netn(c.net_a).to_string(), self.cap_seg_text(i), la]
                };
                rows.push((PSel::Cap(i), c.farads * 1e12, t));
            }
        } else if let Some(seg) = self.seg.as_ref() {
            // one row per segment: the R of a wire or via, and the capacitor sides whose point lies on it
            for (i, sg) in seg.segs.iter().enumerate() {
                let cs = &self.piece_caps[i];
                let r = sg.res.map(|k| lc.res[k].ohms);
                if r.is_none() && cs.is_empty() && !self.pempty {
                    continue;
                }
                let (mut cg, mut cc, mut ncc) = (0.0, 0.0, 0);
                for &(k, _) in cs {
                    if lc.caps[k].coupling {
                        cc += lc.caps[k].farads * 0.5;
                        ncc += 1;
                    } else {
                        cg += lc.caps[k].farads;
                    }
                }
                let kind = match sg.kind {
                    crate::lpe::SegKind::Wire => tr("wire"),
                    crate::lpe::SegKind::Via => tr("via"),
                    crate::lpe::SegKind::Patch => tr("patch"),
                    crate::lpe::SegKind::Path => tr("path"),
                };
                let size = if sg.kind == crate::lpe::SegKind::Wire {
                    let (len, w) = crate::lpe::seg_size_um(sg, self.mpd);
                    format!("{:.3} x {:.3} µm", len, w)
                } else {
                    String::new()
                };
                let t = format!("{:<30} {:<6} {:<17} R {:>11}   C {:>11}   Cc {:>3} {:>11}", self.seg_label(i), kind, size, r.map(fmt_ohm).unwrap_or_else(|| "-".into()), fmt_f(cg), ncc, fmt_f(cc));
                rows.push((PSel::Piece(i), (cg + cc) * 1e12 + r.unwrap_or(0.0) * 1e-9, vec![t]));
            }
        }
        if !f.is_empty() {
            rows.retain(|r| r.2.join(" ").to_lowercase().contains(&f));
        }
        rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ui.small(trf!("{} shown, largest first{}", rows.len(), if self.pview == 1 { tr(" (by total C)") } else { "" }));
        let mut pick = None;
        let h = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;
        let font = egui::TextStyle::Monospace.resolve(ui.style());
        let cw = ui.painter().layout_no_wrap("0".into(), font.clone(), Color32::WHITE).size().x;
        // column starts in characters: kind, name and value | net(s) | segment | layer(s)
        const COLS: [f32; 4] = [0.0, 26.0, 50.0, 78.0];
        egui::ScrollArea::vertical().id_salt(("prows", self.pview)).max_height((ui.available_height() - 120.0).max(120.0)).auto_shrink([false, false]).show_rows(ui, h, rows.len(), |ui, range| {
            for (ps, _, cells) in &rows[range] {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), egui::Sense::click());
                let sel = self.psel == Some(*ps);
                let vis = ui.visuals();
                if sel {
                    ui.painter().rect_filled(rect, 2.0, vis.selection.bg_fill);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 2.0, vis.widgets.hovered.weak_bg_fill);
                }
                let col = if sel { vis.selection.stroke.color } else { vis.text_color() };
                for (k, cell) in cells.iter().enumerate() {
                    let x = rect.left() + 4.0 + COLS[k.min(3)] * cw;
                    let right = if k + 1 < cells.len() && k + 1 < COLS.len() { rect.left() + 4.0 + COLS[k + 1] * cw - cw * 0.5 } else { rect.right() };
                    let clip = Rect::from_min_max(Pos2::new(x, rect.top()), Pos2::new(right.max(x), rect.bottom()));
                    ui.painter_at(clip).text(Pos2::new(x, rect.center().y), egui::Align2::LEFT_CENTER, cell, font.clone(), col);
                }
                if resp.clicked() {
                    pick = Some(*ps);
                }
            }
        });
        // what is picked: why an element is on its layer and segment, or the elements of a segment
        let mut pick2 = None;
        if let Some(ps) = self.psel {
            ui.separator();
            match ps {
                PSel::Res(i) => {
                    if let Some(r) = lc.res.get(i) {
                        ui.label(trf!("{}: {} between {} and {} · segment {}", short(&r.name), fmt_ohm(r.ohms), lc.nodes[r.a].name, lc.nodes[r.b].name, self.res_seg_text(i)));
                        // how its two ends were placed, and why it has no segment
                        if let Some(seg) = self.seg.as_ref() {
                            let how = |n: usize| match seg.from.get(n) {
                                Some(crate::lpe::PosFrom::Own) => tr("its point in the netlist"),
                                Some(crate::lpe::PosFrom::Device) => tr("the terminal of its layout device"),
                                Some(crate::lpe::PosFrom::Resistors) => tr("placed between its R neighbours by the R values"),
                                _ => tr("no position"),
                            };
                            match seg.res.get(i) {
                                Some(Ok(_)) => {
                                    ui.small(trf!("ends: {} ({}), {} ({})", lc.nodes[r.a].name, how(r.a), lc.nodes[r.b].name, how(r.b)));
                                }
                                Some(Err(why)) => {
                                    ui.small(trf!("no segment: {}", why));
                                }
                                None => {}
                            }
                        }
                        if let Some(rl) = attr.and_then(|a| a.res.get(i)) {
                            ui.small(&rl.how);
                        }
                    }
                }
                PSel::Cap(i) => {
                    if let Some(c) = lc.caps.get(i) {
                        let b = c.b.map(|b| lc.nodes[b].name.clone()).unwrap_or_else(|| tr("ground").into());
                        ui.label(trf!("{}: {} between {} and {} · segment {}", short(&c.name), fmt_f(c.farads), lc.nodes[c.a].name, b, self.cap_seg_text(i)));
                        if let Some(cl) = attr.and_then(|a| a.caps.get(i)) {
                            ui.small(&cl.how);
                        }
                    }
                }
                PSel::Piece(p) => {
                    ui.label(trf!("segment {}: {} R, {} capacitor sides (click one to show it)", self.seg_label(p), self.piece_res.get(p).map_or(0, |v| v.len()), self.piece_caps.get(p).map_or(0, |v| v.len())));
                    ui.horizontal_wrapped(|ui| {
                        for &k in self.piece_res.get(p).into_iter().flatten() {
                            if ui.small_button(format!("{} {}", short(&lc.res[k].name), fmt_ohm(lc.res[k].ohms))).clicked() {
                                pick2 = Some(PSel::Res(k));
                            }
                        }
                        for &(k, _) in self.piece_caps.get(p).into_iter().flatten() {
                            if ui.small_button(format!("{} {}", short(&lc.caps[k].name), fmt_f(lc.caps[k].farads))).clicked() {
                                pick2 = Some(PSel::Cap(k));
                            }
                        }
                    });
                }
            }
        }
        if let Some(p) = pick.or(pick2) {
            self.psel = if self.psel == Some(p) && pick2.is_none() { None } else { Some(p) };
            if self.psel.is_some() {
                self.ask.push(CellAsk::Focus(CellView::Layout));
            }
        }
    }

    /// a LEF pin name as the schematic spells it (CDL port names may differ in case)
    fn port_name(&self, pin: &str) -> String {
        self.sc.as_ref().and_then(|sc| sc.ports.iter().find(|p| p.eq_ignore_ascii_case(pin)).cloned()).unwrap_or_else(|| pin.to_string())
    }

    fn xtor_canvas(&mut self, ui: &mut egui::Ui, rect: Rect, ctx: &egui::Context, col: &Colors) {
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let (xl, sc) = match (&self.xl, &self.sc) {
            (Some(a), Some(b)) => (a, b),
            _ => return,
        };
        if self.xfit {
            let z = (rect.width() / xl.size.x).min(rect.height() / xl.size.y).min(2.2) * 0.95;
            self.xview.zoom = z;
            self.xview.pan = Vec2::new(xl.size.x / 2.0 - rect.width() / (2.0 * z), xl.size.y / 2.0 - rect.height() / (2.0 * z));
            self.xfit = false;
        }
        if let Some(p) = self.xcenter.take() {
            self.xview.pan = p.to_vec2() - Vec2::new(rect.width(), rect.height()) / (2.0 * self.xview.zoom);
        }
        if resp.dragged_by(egui::PointerButton::Middle) {
            self.xview.pan -= resp.drag_delta() / self.xview.zoom;
        }
        let mut hover = None;
        if let Some(pos) = resp.hover_pos() {
            let (zf, pan) = crate::nav::wheel(ctx);
            if zf != 1.0 {
                let before = self.xview.to_world(rect, pos);
                self.xview.zoom = (self.xview.zoom * zf).clamp(0.05, 20.0);
                let after = self.xview.to_world(rect, pos);
                self.xview.pan += before - after;
            }
            self.xview.pan -= pan / self.xview.zoom;
            let w = self.xview.to_world(rect, pos);
            hover = xtor::hit_test(xl, sc, w, 4.0 / self.xview.zoom).map(|h| match h {
                XHit::Dev(n) => CSel::Dev(n),
                XHit::Net(n) => CSel::Net(n),
            });
            if resp.clicked() || (resp.secondary_clicked() && hover.is_some()) {
                self.sel = hover.clone();
            }
        }
        let xs = self.xsel();
        let mut painter = ui.painter_at(rect);
        xtor::paint(&mut painter, rect, &self.xview, xl, sc, xs.as_ref(), col);
        let (can_jump, together) = (self.flat.is_some(), self.side_by_side);
        let (mut jump, mut both) = (false, false);
        resp.context_menu(|ui| {
            if ui.add_enabled(can_jump, egui::Button::new(tr("Show in layout"))).clicked() {
                jump = true;
                ui.close();
            }
            if ui.add(egui::Button::selectable(together, tr("Schematic and layout side by side"))).clicked() {
                both = true;
                ui.close();
            }
        });
        if jump {
            self.ask.push(CellAsk::Focus(CellView::Layout));
        }
        if both {
            self.ask.push(CellAsk::SideBySide);
        }
        if let Some(h) = &hover {
            let t = self.label(h);
            resp.on_hover_text_at_pointer(t);
        }
        self.hover = hover;
    }

    fn gds_canvas(&mut self, ui: &mut egui::Ui, rect: Rect, ctx: &egui::Context, col: &Colors) {
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        if self.flat.is_none() {
            return;
        }
        if self.gfit {
            self.fit_gds(rect);
        }
        let c = rect.center();
        let to_world = |o: [f64; 2], s: f64, p: Pos2| [o[0] + (p.x - c.x) as f64 / s, o[1] - (p.y - c.y) as f64 / s];
        if resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            self.gorigin[0] -= d.x as f64 / self.gscale;
            self.gorigin[1] += d.y as f64 / self.gscale;
        }
        let mut hover = None;
        if let Some(pos) = resp.hover_pos() {
            let (zf, pan) = crate::nav::wheel(ctx);
            if zf != 1.0 {
                let before = to_world(self.gorigin, self.gscale, pos);
                self.gscale *= zf as f64;
                let after = to_world(self.gorigin, self.gscale, pos);
                self.gorigin[0] += before[0] - after[0];
                self.gorigin[1] += before[1] - after[1];
            }
            self.gorigin[0] -= pan.x as f64 / self.gscale;
            self.gorigin[1] += pan.y as f64 / self.gscale;
            let w = to_world(self.gorigin, self.gscale, pos);
            hover = self.from_layout(self.gpick(w[0], w[1], 2.0 / self.gscale));
            if resp.clicked() || (resp.secondary_clicked() && hover.is_some()) {
                let s = hover.clone();
                self.select(s, true);
            }
        }
        let bg = ui.visuals().extreme_bg_color;
        let fg = ui.visuals().text_color();
        let dark = ui.visuals().dark_mode;
        let mut painter = ui.painter_at(rect);
        self.paint_gds(&mut painter, rect, bg, fg, dark, col.hl);
        self.paint_parasitics(&painter, rect);
        let (can_jump, together) = (self.sc.is_some(), self.side_by_side);
        let (mut jump, mut both) = (false, false);
        resp.context_menu(|ui| {
            if ui.add_enabled(can_jump, egui::Button::new(tr("Show in schematic"))).clicked() {
                jump = true;
                ui.close();
            }
            if ui.add(egui::Button::selectable(together, tr("Schematic and layout side by side"))).clicked() {
                both = true;
                ui.close();
            }
        });
        if jump {
            self.ask.push(CellAsk::Focus(CellView::Schematic));
        }
        if both {
            self.ask.push(CellAsk::SideBySide);
        }
        if let Some(h) = &hover {
            let t = self.label(h);
            resp.on_hover_text_at_pointer(t);
        }
        if hover.is_some() {
            self.hover = hover;
        }
    }

    pub fn fit_gds(&mut self, rect: Rect) {
        if let Some(f) = &self.flat {
            let b = bbox(&f.polys);
            let bw = (b[2] - b[0]).max(1) as f64;
            let bh = (b[3] - b[1]).max(1) as f64;
            self.gscale = (rect.width() as f64 / bw).min(rect.height() as f64 / bh) * 0.9;
            self.gorigin = [(b[0] + b[2]) as f64 / 2.0, (b[1] + b[3]) as f64 / 2.0];
        }
        self.gfit = false;
    }

    /// the GDS cell with fills, hatches, outlines, highlight and labels
    pub fn paint_gds(&self, cv: &mut dyn Canvas, rect: Rect, bg: Color32, fg: Color32, dark: bool, acc: Color32) {
        let flat = match &self.flat {
            Some(f) => f,
            None => return,
        };
        let c = rect.center();
        let (o, s) = (self.gorigin, self.gscale);
        let sx = |x: f64| c.x + ((x - o[0]) * s) as f32;
        let sy = |y: f64| c.y - ((y - o[1]) * s) as f32;
        let quad = |r: Rect| vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
        cv.closed(quad(rect), Some(bg), Stroke::NONE);
        for l in &self.layers {
            if !l.visible {
                continue;
            }
            let color = if l.cut && dark { hexc(0xced4da) } else { l.color };
            if !l.outline {
                let a = if l.cut { 0.6 } else if l.base == "Active" || l.base == "fin" { 0.16 } else { 0.2 };
                let fill = color.gamma_multiply(a);
                let hatch = !l.cut && l.base != "Active" && l.base != "fin";
                for r in &l.rects {
                    let sr = Rect::from_min_max(Pos2::new(sx(r[0] as f64), sy(r[3] as f64)), Pos2::new(sx(r[2] as f64), sy(r[1] as f64)));
                    if !sr.intersects(rect) {
                        continue;
                    }
                    let sr = sr.intersect(rect);
                    cv.closed(quad(sr), Some(fill), Stroke::NONE);
                    if hatch {
                        draw_hatch(cv, sr, l.dir, color.gamma_multiply(0.85));
                    }
                }
            }
            let st = Stroke::new(if l.outline { 1.0 } else { 1.25 }, color);
            for p in &l.polys {
                let pts: Vec<Pos2> = (0..p.len() / 2).map(|k| Pos2::new(sx(p[2 * k] as f64), sy(p[2 * k + 1] as f64))).collect();
                if pts.len() < 2 {
                    continue;
                }
                let mut closed = pts.clone();
                closed.push(pts[0]);
                if l.dashed {
                    cv.dashed(closed, st);
                } else {
                    cv.line(closed, st);
                }
            }
        }
        let (hn, hd, ha) = self.highlight();
        if !hn.is_empty() || !hd.is_empty() || !ha.is_empty() {
            cv.closed(quad(rect), Some(bg.gamma_multiply(0.55)), Stroke::NONE);
            for (rects, alpha, lw) in [(&hn, 0.5f32, 1.5f32), (&ha, 0.4, 1.5), (&hd, 0.85, 2.5)] {
                for r in rects.iter() {
                    let sr = Rect::from_min_max(Pos2::new(sx(r[0] as f64), sy(r[3] as f64)), Pos2::new(sx(r[2] as f64), sy(r[1] as f64)));
                    cv.closed(quad(sr), Some(acc.gamma_multiply(alpha)), Stroke::new(lw, acc));
                }
            }
        }
        for t in &flat.texts {
            let vis = self.layers.iter().find(|l| l.key == (t.layer as i32, t.dt as i32)).map_or(true, |l| l.visible);
            if !vis {
                continue;
            }
            let p = Pos2::new(sx(t.x as f64), sy(t.y as f64));
            if !rect.contains(p) {
                continue;
            }
            cv.circle(p, 1.8, Some(fg), Stroke::NONE);
            cv.text(p + Vec2::new(3.0, 0.0), egui::Align2::LEFT_CENTER, &t.string, 11.0, fg);
        }
    }

    /// load the files synchronously (command-line checks)
    pub fn load_sync(&mut self) -> Result<(), String> {
        let rd = |p: &PathBuf| crate::io::read_all(p).map_err(|e| format!("{}: {}", p.display(), e));
        if let Some(p) = self.files.cdl.clone() {
            self.cdl = Some(parse_cdl(&String::from_utf8_lossy(&rd(&p)?), &p.display().to_string()));
        }
        if let Some(p) = self.files.gds.clone() {
            self.gds = Some(parse_gds(&rd(&p)?, &p.display().to_string())?);
        }
        if let Some(p) = self.files.layermap.clone() {
            self.lmap = parse_layer_map(&String::from_utf8_lossy(&rd(&p)?));
        }
        self.auto_preset();
        self.fill_names();
        Ok(())
    }

    /// the cell tab (schematic left, GDS right) as one SVG, with an optional selection
    pub fn to_svg(&mut self, w: f32, h: f32, sel: Option<CSel>, col: &Colors) -> String {
        let mut svg = crate::schematic::Svg { body: String::new() };
        let half = w / 2.0;
        let xr = Rect::from_min_size(Pos2::ZERO, Vec2::new(half - 1.0, h));
        let gr = Rect::from_min_max(Pos2::new(half + 1.0, 0.0), Pos2::new(w, h));
        self.sel = sel;
        if let (Some(xl), Some(sc)) = (&self.xl, &self.sc) {
            let z = (xr.width() / xl.size.x).min(xr.height() / xl.size.y).min(2.2) * 0.95;
            let view = View { pan: Vec2::new(xl.size.x / 2.0 - xr.width() / (2.0 * z), xl.size.y / 2.0 - xr.height() / (2.0 * z)), zoom: z };
            let xs = self.xsel();
            xtor::paint(&mut svg, xr, &view, xl, sc, xs.as_ref(), col);
        }
        self.fit_gds(gr);
        self.paint_gds(&mut svg, gr, Color32::WHITE, Color32::from_rgb(0x18, 0x22, 0x2e), false, col.hl);
        format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{:.0}" height="{:.0}" viewBox="0 0 {:.0} {:.0}"><rect width="100%" height="100%" fill="#ffffff"/>{}<path d="M{:.0},0V{:.0}" stroke="#ccc"/></svg>"##, w, h, w, h, svg.body, half, h)
    }
}

/// diagonal hatch inside a screen rectangle, 8 px apart, aligned to the screen so neighbours line up
fn draw_hatch(p: &mut dyn Canvas, r: Rect, dir: u8, color: Color32) {
    if r.width() < 6.0 || r.height() < 6.0 {
        return;
    }
    let st = Stroke::new(1.1, color);
    let (x0, y0, x1, y1) = (r.left(), r.top(), r.right(), r.bottom());
    if dir == 1 {
        // x + y = k
        let mut k = ((x0 + y0) / 8.0).ceil() * 8.0;
        while k <= x1 + y1 {
            let a = Pos2::new((k - y1).max(x0), 0.0);
            let a = Pos2::new(a.x, k - a.x);
            let b = Pos2::new((k - y0).min(x1), 0.0);
            let b = Pos2::new(b.x, k - b.x);
            p.line(vec![a, b], st);
            k += 8.0;
        }
    } else {
        // x - y = k
        let mut k = ((x0 - y1) / 8.0).ceil() * 8.0;
        while k <= x1 - y0 {
            let a = Pos2::new((k + y0).max(x0), 0.0);
            let a = Pos2::new(a.x, a.x - k);
            let b = Pos2::new((k + y1).min(x1), 0.0);
            let b = Pos2::new(b.x, b.x - k);
            p.line(vec![a, b], st);
            k += 8.0;
        }
    }
}
