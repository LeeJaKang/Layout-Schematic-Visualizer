//! Built-in PDK presets (a layer map plus extraction rules for one standard-cell library family) and
//! detection of the preset from the GDS layer/datatype pairs a file uses. Presets whose data is not clearly
//! open source are not built in: they live in `.lsvpdk` files read at run time (see `load_file`).
use crate::extract::{parse_layer_map, parse_rules};
use crate::gds::GdsLib;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub name: &'static str,
    pub layermap: &'static str,
    pub rules: &'static str,
}

/// ASAP7 layer numbers (asap7_TechLib.layermap of the ASAP7 PDK), used when no layer map is loaded
pub const ASAP7_LAYERMAP: &str = "well drawing 1 0\nfin drawing 2 0\nGate drawing 7 0\nDummy drawing 8 0\nGCut drawing 10 0\nActive drawing 11 0
Nselect drawing 12 0\nPselect drawing 13 0\nLIG drawing 16 0\nLISD drawing 17 0\nV0 drawing 18 0\nM1 drawing 19 0
V1 drawing 21 0\nM2 drawing 20 0\nV2 drawing 25 0\nM3 drawing 30 0\nV3 drawing 35 0\nM4 drawing 40 0\nV4 drawing 45 0
M5 drawing 50 0\nV5 drawing 55 0\nM6 drawing 60 0\nV6 drawing 65 0\nM7 drawing 70 0\nV7 drawing 75 0\nM8 drawing 80 0
V8 drawing 85 0\nM9 drawing 90 0\nV9 drawing 95 0\nSDT drawing 88 0\nSLVT drawing 97 0\nLVT drawing 98 0
SRAMDRC drawing 99 0\nBOUNDARY drawing 100 0\nTEXT drawing 101 0\nSRAMVT drawing 110 0
well pin 1 251\nP_SUB pin 3 251\nGate pin 7 251\nLIG pin 16 251\nLISD pin 17 251\nM1 pin 19 251\nM2 pin 20 251
M3 pin 30 251\nM4 pin 40 251\nM5 pin 50 251\nM6 pin 60 251\nM7 pin 70 251\nM8 pin 80 251\nM9 pin 90 251
Gate label 7 2\nLIG label 16 2\nLISD label 17 2\nM1 label 19 2\nM2 label 20 2\nM3 label 30 2\nM4 label 40 2
M5 label 50 2\nM6 label 60 2\nM7 label 70 2\nM8 label 80 2\nM9 label 90 2";

/// ASAP7 (asap7sc7p5t, 1x GDS) defaults. Traps they encode: VDD/VSS texts on 1/251 and 3/251 name the
/// PMOS well and the NMOS substrate, not M1; LISD reaches diffusion only through SDT; gate cut removes
/// the gate over active; series nodes may be diffusion only, labelled on Active; LIG over LISD is a contact.
pub const ASAP7_RULES: &str = "# ASAP7 extraction rules (layer names from asap7_TechLib.layermap)
layer gate    = Gate not GCut
layer channel = gate and Active
layer diff    = Active not Gate
layer sdc     = LISD and SDT
conduct M1 M2 M3 V0 V1 V2 LIG LISD gate diff sdc well
connect LIG gate
connect LIG LISD
connect V0 LIG
connect V0 LISD
connect V0 M1
connect LISD sdc
connect sdc diff
connect M1 V1
connect V1 M2
connect M2 V2
connect V2 M3
device nmos channel gate=gate sd=diff when=Nselect bulk=@substrate fin=fin
device pmos channel gate=gate sd=diff when=Pselect bulk=well fin=fin
label 19/251 -> M1
label 20/251 -> M2
label 30/251 -> M3
label 11/0 -> diff
label 1/251 -> well
label 3/251 -> @substrate
join-same-name
";

/// FreePDK45 layers (FreePDK45.lyp of OpenROAD-flow-scripts platforms/nangate45, Apache-2.0) as used by the
/// NanGate Open Cell Library GDS.
pub const NG45_LAYERMAP: &str = "active drawing 1 0\npwell drawing 2 0\nnwell drawing 3 0\nnimplant drawing 4 0
pimplant drawing 5 0\nvtg drawing 6 0\nvth drawing 7 0\nthkox drawing 8 0\npoly drawing 9 0\ncontact drawing 10 0
metal1 drawing 11 0\nvia1 drawing 12 0\nmetal2 drawing 13 0\nvia2 drawing 14 0\nmetal3 drawing 15 0\nvia3 drawing 16 0
metal4 drawing 17 0\nvia4 drawing 18 0\nmetal5 drawing 19 0\nvia5 drawing 20 0\nmetal6 drawing 21 0\nvia6 drawing 22 0
metal7 drawing 23 0\nvia7 drawing 24 0\nmetal8 drawing 25 0\nvia8 drawing 26 0\nmetal9 drawing 27 0\nvia9 drawing 28 0
metal10 drawing 29 0\nOUTLINE drawing 235 0\ncomment drawing 239 0";

/// NanGate Open Cell Library (FreePDK45). Pin texts sit on metal1 drawing (11/0). The cells carry no well
/// taps (the rows get TAPCELL_X1 / WELLTAP cells), so the wells float inside a cell and the bulk is not
/// compared; device type comes from the implant.
pub const NG45_RULES: &str = "# NanGate45 / FreePDK45 extraction rules
layer channel = poly and active
layer diff    = active not poly
conduct metal1 metal2 contact via1 poly diff
connect contact poly
connect contact diff
connect contact metal1
connect metal1 via1
connect via1 metal2
device nmos channel gate=poly sd=diff when=nimplant size=wl
device pmos channel gate=poly sd=diff when=pimplant size=wl
label 11/0 -> metal1
label 13/0 -> metal2
join-same-name
";

/// SKY130 layers (sky130hd.lyp of OpenROAD-flow-scripts platforms/sky130hd, from the open SKY130 PDK,
/// Apache-2.0)
pub const SKY130_LAYERMAP: &str = "nwell drawing 64 20\nnwell label 64 5\nnwell pin 64 16\npwell drawing 64 44\npwell label 64 59
pwell pin 122 16\ndnwell drawing 64 18\ndiff drawing 65 20\ntap drawing 65 44\npoly drawing 66 20\nnpc drawing 95 20
nsdm drawing 93 44\npsdm drawing 94 20\nhvtp drawing 78 44\nlvtn drawing 125 44\nhvi drawing 75 20\nlicon1 drawing 66 44
li1 drawing 67 20\nli1 label 67 5\nli1 pin 67 16\nmcon drawing 67 44\nmet1 drawing 68 20\nmet1 label 68 5\nmet1 pin 68 16
via drawing 68 44\nmet2 drawing 69 20\nmet2 label 69 5\nmet2 pin 69 16\nvia2 drawing 69 44\nmet3 drawing 70 20
met3 label 70 5\nmet3 pin 70 16\nvia3 drawing 70 44\nmet4 drawing 71 20\nmet4 label 71 5\nmet4 pin 71 16\nvia4 drawing 71 44
met5 drawing 72 20\nmet5 label 72 5\nmet5 pin 72 16\nareaid_sc identifier 81 4\nprBoundary drawing 235 4
prBoundary boundary 236 0\ntext drawing 83 44";

/// SKY130 standard cells (sky130_fd_sc_hd, hs, ...). The CDL writes w= and l= in microns without a unit.
/// VPB / VNB texts on nwell.label and pwell.label name the n-well and the p-substrate; tap (65/44) is a
/// separate diffusion from diff (65/20), and n taps tie the n-well, p taps the substrate.
pub const SKY130_RULES: &str = "# SKY130 standard-cell extraction rules
layer channel = poly and diff
layer sd      = diff not poly
layer ntap    = tap and nsdm
layer ptap    = tap and psdm
conduct met1 met2 met3 met4 met5 mcon via via2 via3 via4 li1 licon1 poly sd ntap ptap nwell
connect licon1 poly
connect licon1 sd
connect licon1 ntap
connect licon1 ptap
connect licon1 li1
connect li1 mcon
connect mcon met1
connect met1 via
connect via met2
connect met2 via2
connect via2 met3
connect met3 via3
connect via3 met4
connect met4 via4
connect via4 met5
connect nwell ntap
connect @substrate ptap
device nmos channel gate=poly sd=sd when=nsdm bulk=@substrate size=wl
device pmos channel gate=poly sd=sd when=psdm bulk=nwell size=wl
label 67/5 -> li1
label 68/5 -> met1
label 69/5 -> met2
label 70/5 -> met3
label 71/5 -> met4
label 72/5 -> met5
label 64/5 -> nwell
label 64/59 -> @substrate
cdl-scale 1e-6
join-same-name
";

/// IHP SG13G2 layers (layers_definitions.lvs of the IHP-Open-PDK KLayout LVS deck, Apache-2.0)
pub const IHP_SG13G2_LAYERMAP: &str = "Activ drawing 1 0\nActiv pin 1 2\nGatPoly drawing 5 0\nGatPoly pin 5 2\nCont drawing 6 0
nSD drawing 7 0\nMetal1 drawing 8 0\nMetal1 pin 8 2\nMetal1 text 8 25\npSD drawing 14 0\nVia1 drawing 19 0
Metal2 drawing 10 0\nMetal2 pin 10 2\nMetal2 text 10 25\nVia2 drawing 29 0\nMetal3 drawing 30 0\nMetal3 pin 30 2
Metal3 text 30 25\nNWell drawing 31 0\nNWell pin 31 2\nSubstrate drawing 40 0\nThickGateOx drawing 44 0\nTEXT drawing 63 0
Recog diode 99 31\nprBoundary drawing 189 0\nprBoundary boundary 189 4";

/// IHP SG13G2 standard cells (sg13g2_stdcell). n+ is Activ outside pSD; an n+ tap in the NWell ties the
/// well, a p+ tap outside it the substrate (as in the IHP deck's ntap / ptap). Source diffusion butted
/// against a tap is one net with it through the silicide (the deck's nsd_ptap_abutt / psd_ntap_abutt):
/// several flip-flops reach VSS/VDD only that way. Antenna diodes (Recog diode) are not extracted, so
/// their CDL D elements are reported as not compared.
pub const IHP_SG13G2_RULES: &str = "# IHP SG13G2 standard-cell extraction rules
layer channel = GatPoly and Activ
layer nact    = Activ not pSD
layer pact    = Activ and pSD
layer nsd     = Activ not GatPoly not pSD not NWell
layer psd     = pact and NWell not GatPoly
layer sd      = nsd or psd
layer ntap   = Activ not pSD and NWell not GatPoly
layer ptap    = Activ and pSD not NWell not GatPoly
conduct Metal1 Metal2 Metal3 Cont Via1 Via2 GatPoly sd ntap ptap NWell
connect Cont GatPoly
connect Cont sd
connect Cont ntap
connect Cont ptap
connect Cont Metal1
connect Metal1 Via1
connect Via1 Metal2
connect Metal2 Via2
connect Via2 Metal3
connect NWell ntap
connect @substrate ptap
abut sd ptap
abut sd ntap
device nmos channel gate=GatPoly sd=sd when=nact bulk=@substrate size=wl
device pmos channel gate=GatPoly sd=sd when=pact bulk=NWell size=wl
label 8/25 -> Metal1
label 10/25 -> Metal2
label 30/25 -> Metal3
join-same-name
";

/// GF180MCU layers (gf180mcu.lvs of the open GF180MCU PDK device library gf180mcu_fd_pr, Apache-2.0)
pub const GF180MCU_LAYERMAP: &str = "PR_bndry drawing 0 0\nDNWELL drawing 12 0\nNwell drawing 21 0\nCOMP drawing 22 0
Poly2 drawing 30 0\nPplus drawing 31 0\nNplus drawing 32 0\nContact drawing 33 0\nMetal1 drawing 34 0\nMetal1 label 34 10
Via1 drawing 35 0\nMetal2 drawing 36 0\nMetal2 label 36 10\nVia2 drawing 38 0\nMetal3 drawing 42 0\nMetal3 label 42 10
Dualgate drawing 55 0\nV5_XTOR drawing 112 1\nLVPWELL drawing 204 0";

/// GF180MCU standard cells (gf180mcu_fd_sc_mcu7t5v0 / 9t5v0). As in the GF180 deck, an NMOS outside DNWELL
/// has the substrate as bulk (p taps tie it) and a PMOS the Nwell (n taps); the cells carry no taps, so the
/// substrate and the well are the separate VPW / VNW ports of the CDL.
pub const GF180MCU_RULES: &str = "# GF180MCU standard-cell extraction rules
layer channel = Poly2 and COMP
layer ncomp   = COMP and Nplus
layer pcomp   = COMP and Pplus
layer nsd     = ncomp not Nwell not Poly2
layer psd     = pcomp and Nwell not Poly2
layer sd      = nsd or psd
layer ntap    = ncomp and Nwell not Poly2
layer ptap    = pcomp not Nwell not Poly2
conduct Metal1 Metal2 Metal3 Contact Via1 Via2 Poly2 sd ntap ptap Nwell
connect Contact Poly2
connect Contact sd
connect Contact ntap
connect Contact ptap
connect Contact Metal1
connect Metal1 Via1
connect Via1 Metal2
connect Metal2 Via2
connect Via2 Metal3
connect Nwell ntap
connect @substrate ptap
device nmos channel gate=Poly2 sd=sd when=Nplus bulk=@substrate size=wl
device pmos channel gate=Poly2 sd=sd when=Pplus bulk=Nwell size=wl
label 34/10 -> Metal1
label 36/10 -> Metal2
label 42/10 -> Metal3
join-same-name
";

/// GT3 3 nm GAAFET layers (gt3_techfile.layermap of the GT3 PDK, BSD-3-Clause)
pub const GT3_LAYERMAP: &str = "NWELL drawing 1 0\nACT drawing 2 0\nGATE drawing 3 0\nDUMMY drawing 4 0\nGCUT drawing 5 0
NSEL drawing 6 0\nPSEL drawing 7 0\nBPR drawing 8 0\nBPR pin 8 251\nVBPR drawing 9 0\nSDCON drawing 10 0\nVSD drawing 11 0
VG drawing 12 0\nM0 drawing 20 0\nM0 pin 20 251\nV0 drawing 22 0\nM1 drawing 25 0\nM1 pin 25 251\nV1 drawing 27 0
M2 drawing 30 0\nM2 pin 30 251\nV2 drawing 32 0\nM3 drawing 35 0\nBOUNDARY drawing 235 250";

/// GT3 standard cells: poly = GATE not GCUT not DUMMY, devices are 3-terminal (no bulk), the buried power
/// rail (BPR) reaches source/drain contacts through VBPR and SDCON, and W is the drawn nanosheet (ACT) width
/// under the gate.
pub const GT3_RULES: &str = "# GT3 standard-cell extraction rules
layer pg    = GATE not GCUT
layer poly  = pg not DUMMY
layer nact  = ACT not NWELL
layer pact  = ACT and NWELL
layer nns   = nact and NSEL
layer pns   = pact and PSEL
layer chn   = poly and nns
layer chp   = poly and pns
layer nsd   = nns not poly not DUMMY
layer psd   = pns not poly not DUMMY
layer sd    = nsd or psd
conduct M0 M1 M2 V0 V1 VG VSD SDCON BPR VBPR poly sd
connect poly VG
connect VG M0
connect SDCON sd
connect SDCON VSD
connect VSD M0
connect M0 V0
connect V0 M1
connect M1 V1
connect V1 M2
connect BPR VBPR
connect VBPR SDCON
device nmos chn gate=poly sd=sd size=wl
device pmos chp gate=poly sd=sd size=wl
label 8/251 -> BPR
label 20/251 -> M0
label 25/251 -> M1
label 30/251 -> M2
join-same-name
";

pub const PRESETS: [Preset; 6] = [
    Preset { name: "ASAP7", layermap: ASAP7_LAYERMAP, rules: ASAP7_RULES },
    Preset { name: "NG45", layermap: NG45_LAYERMAP, rules: NG45_RULES },
    Preset { name: "SKY130", layermap: SKY130_LAYERMAP, rules: SKY130_RULES },
    Preset { name: "IHP-SG13G2", layermap: IHP_SG13G2_LAYERMAP, rules: IHP_SG13G2_RULES },
    Preset { name: "GF180MCU", layermap: GF180MCU_LAYERMAP, rules: GF180MCU_RULES },
    Preset { name: "GT3", layermap: GT3_LAYERMAP, rules: GT3_RULES },
];

/// the built-in presets, ASAP7 first
pub fn presets() -> &'static [Preset] {
    &PRESETS
}

/// every layer/datatype pair a library draws or writes text on
pub fn layers_used(lib: &GdsLib) -> HashSet<(i32, i32)> {
    let mut s = HashSet::new();
    for c in lib.cells.values() {
        for p in &c.polys {
            s.insert((p.layer as i32, p.dt as i32));
        }
        for p in &c.paths {
            s.insert((p.layer as i32, p.dt as i32));
        }
        for t in &c.texts {
            s.insert((t.layer as i32, t.dt as i32));
        }
    }
    s
}

/// layer/datatype pairs of the preset's map, and the pairs its device recognition reads (the primitive
/// layers of `layer` expressions and of device rules; upper metals are left out because a cell library
/// need not draw them)
pub fn preset_layers(p: &Preset) -> (HashSet<(i32, i32)>, HashSet<(i32, i32)>) {
    let lm = parse_layer_map(p.layermap);
    let map: HashSet<(i32, i32)> = lm.by_num.keys().copied().collect();
    let r = parse_rules(p.rules);
    let derived: HashSet<&str> = r.layers.iter().map(|l| l.0.as_str()).collect();
    let mut names: Vec<&str> = vec![];
    for l in &r.layers {
        names.extend(l.1.iter().map(|s| s.as_str()).filter(|t| !matches!(t.to_lowercase().as_str(), "and" | "or" | "not")));
    }
    for d in &r.devices {
        names.push(&d.channel);
        for k in ["gate", "sd", "when", "bulk", "fin"] {
            if let Some(v) = d.kv.get(k) {
                names.push(v);
            }
        }
    }
    let mut rule = HashSet::new();
    for nm in names.iter().copied() {
        if derived.contains(nm) || nm.starts_with('@') {
            continue;
        }
        let ld = nm.split_once('/').and_then(|(a, b)| Some((a.parse::<i32>().ok()?, b.parse::<i32>().ok()?)));
        if let Some(k) = ld.or_else(|| lm.by_name.get(nm).copied()) {
            rule.insert(k);
        }
    }
    (map, rule)
}

/// how well a preset explains a set of used layers: (share of used pairs in the preset map) x
/// (share of the pairs its rules read that the file uses)
pub fn score(p: &Preset, used: &HashSet<(i32, i32)>) -> f64 {
    if used.is_empty() {
        return 0.0;
    }
    let (map, rule) = preset_layers(p);
    let cov = used.iter().filter(|k| map.contains(k)).count() as f64 / used.len() as f64;
    let req = if rule.is_empty() { 0.0 } else { rule.iter().filter(|k| used.contains(k)).count() as f64 / rule.len() as f64 };
    cov * req
}

/// the best preset of `list` for these used layers; None when no preset explains at least half of them
pub fn detect_among(list: &[Preset], used: &HashSet<(i32, i32)>) -> Option<usize> {
    let mut best: Option<(usize, f64)> = None;
    for (i, p) in list.iter().enumerate() {
        let s = score(p, used);
        if best.map_or(true, |b| s > b.1) {
            best = Some((i, s));
        }
    }
    best.filter(|b| b.1 >= 0.5).map(|b| b.0)
}

/// pick a built-in preset from the GDS layer/datatype pairs a file uses
pub fn detect(layers_used: &HashSet<(i32, i32)>) -> Option<usize> {
    detect_among(presets(), layers_used)
}

pub fn find(list: &[Preset], name: &str) -> Option<usize> {
    list.iter().position(|p| p.name.eq_ignore_ascii_case(name))
}

/// read a run-time preset file: a `name NAME` line, then `[layermap]` and `[rules]` sections holding the
/// two texts verbatim. Used for presets that must not be compiled into the program.
pub fn load_file(path: &std::path::Path) -> Result<Preset, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let (mut name, mut lm, mut rules) = (String::new(), String::new(), String::new());
    let mut sec = 0;
    for line in text.lines() {
        match line.trim() {
            "[layermap]" => sec = 1,
            "[rules]" => sec = 2,
            t => match sec {
                0 => {
                    if let Some(n) = t.strip_prefix("name ") {
                        name = n.trim().to_string();
                    }
                }
                1 => {
                    lm.push_str(line);
                    lm.push('\n');
                }
                _ => {
                    rules.push_str(line);
                    rules.push('\n');
                }
            },
        }
    }
    if name.is_empty() || lm.trim().is_empty() || rules.trim().is_empty() {
        return Err(format!("{}: a preset file needs a name line and [layermap] and [rules] sections", path.display()));
    }
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    Ok(Preset { name: leak(name), layermap: leak(lm), rules: leak(rules) })
}

/// built-in presets followed by every `*.lsvpdk` file in the directories of LSV_PDK_PATH (':'-separated)
pub fn all_presets() -> (Vec<Preset>, Vec<String>) {
    let mut out: Vec<Preset> = presets().to_vec();
    let mut errs = vec![];
    if let Ok(dirs) = std::env::var("LSV_PDK_PATH") {
        for d in dirs.split(':').filter(|d| !d.is_empty()) {
            let mut files: Vec<std::path::PathBuf> = match std::fs::read_dir(d) {
                Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().map_or(false, |x| x == "lsvpdk")).collect(),
                Err(e) => {
                    errs.push(format!("LSV_PDK_PATH {}: {}", d, e));
                    continue;
                }
            };
            files.sort();
            for f in files {
                match load_file(&f) {
                    Ok(p) => out.push(p),
                    Err(e) => errs.push(e),
                }
            }
        }
    }
    (out, errs)
}
