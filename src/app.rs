//! Desktop visualizer window: LEF/DEF layout on the GPU, gate-level schematic, cross-probing both ways.
use crate::cells::analyze_cell;
use crate::cellview::{CellAsk, CellFiles, CellTab, CellView};
use crate::def::{comp_shapes, parse_def_file, Layout};
use crate::gpu::{styles_for, Cam, Frame, Gpu, SchGpu};
use crate::lef::{parse_lef, Lib};
use crate::liberty::parse_liberty;
use crate::logic::describe_gate;
use crate::netlist::{cone_cells, elaborate, pick_top, End, Netlist};
use crate::fold::{self, Fold};
use std::collections::HashSet;
use crate::scene::{slot_of, Inst, Pick, Scene, Show, GROUPS, GROUP_PIN, GROUP_POWER, GROUP_SIGNAL};
use crate::schematic::{self, Hit, NodeKind, SchLayout, View};
use crate::verilog::parse_verilog;
use eframe::egui;
use eframe::egui_wgpu;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Instant;
use crate::i18n::tr;
use crate::trf;

/// designs up to this many cells are drawn whole in the netlist tab; larger ones around the selection
pub const FULL_LIMIT: usize = 60_000;
/// with more cells than this inside the view, the netlist is drawn as the GPU overview
const DETAIL_CELLS: usize = 1500;

pub struct Loaded {
    pub lib: Lib,
    pub lay: Layout,
    pub scene: Scene,
    pub names: Vec<(String, u8, u32)>,
    pub log: String,
}

pub struct NlLoaded {
    pub nl: Netlist,
    pub log: String,
    /// submodules (or instance-name paths) as foldable groups
    pub fold: Fold,
}

/// a folded netlist view laid out in the background
pub struct FoldDone {
    lay: SchLayout,
    cache: fold::Cache,
    focus: Option<Hit>,
    fit: bool,
    ms: u128,
    nl: Arc<NlLoaded>,
}

enum Msg {
    Status(String),
    Done(Box<Loaded>),
    NlDone(Box<NlLoaded>),
    Failed(String),
    SchDone(Box<FoldDone>),
    SpefDone(Box<crate::spef::Spef>, String),
}

#[derive(Default, Clone)]
pub struct Files {
    pub lefs: Vec<PathBuf>,
    pub def: Option<PathBuf>,
    pub libs: Vec<PathBuf>,
    pub netlist: Option<PathBuf>,
    pub top: Option<String>,
    pub cdl: Option<PathBuf>,
    pub gds: Option<PathBuf>,
    pub layermap: Option<PathBuf>,
    pub cell_lef: Option<PathBuf>,
    pub lpe: Option<PathBuf>,
    /// block parasitics (SPEF, such as OpenRCX output), drawn on the block layout
    pub spef: Option<PathBuf>,
    pub cell: Option<String>,
    /// tab to open with: netlist, layout or cell (it then stays there while files load)
    pub tab: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Tab {
    Layout,
    Schematic,
    Cell,
}

// ---------- dock panes ----------
/// every view is a pane of one dock: drag a tab onto another pane's edge to put the two side by side or one
/// above the other, onto its tab bar to stack them as tabs
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Pane {
    Netlist,
    BlockLayout,
    Schematic,
    CellLayout,
    CellLef,
    Parasitics,
    Details,
}
pub const VIEW_PANES: [Pane; 5] = [Pane::Netlist, Pane::BlockLayout, Pane::Schematic, Pane::CellLayout, Pane::CellLef];
impl Pane {
    pub fn title(self) -> &'static str {
        tr(match self {
            Pane::Netlist => "Netlist",
            Pane::BlockLayout => "Block layout",
            Pane::Schematic => "Schematic",
            Pane::CellLayout => "Cell layout",
            Pane::CellLef => "Cell LEF",
            Pane::Parasitics => "Parasitics",
            Pane::Details => "Details",
        })
    }
    /// the view family a pane belongs to (search and the details pane follow it)
    pub fn tab(self) -> Option<Tab> {
        match self {
            Pane::Netlist => Some(Tab::Schematic),
            Pane::BlockLayout => Some(Tab::Layout),
            Pane::Schematic | Pane::CellLayout | Pane::CellLef | Pane::Parasitics => Some(Tab::Cell),
            Pane::Details => None,
        }
    }
    pub fn of_cell(v: CellView) -> Pane {
        match v {
            CellView::Schematic => Pane::Schematic,
            CellView::Layout => Pane::CellLayout,
            CellView::Lef => Pane::CellLef,
        }
    }
    /// name used by --tab and LSV_POP
    pub fn from_name(s: &str) -> Option<Pane> {
        Some(match s.trim().to_lowercase().as_str() {
            "netlist" | "schematic-block" => Pane::Netlist,
            "layout" | "block-layout" => Pane::BlockLayout,
            "cell" | "cell-schematic" | "xtor" => Pane::Schematic,
            "cell-layout" | "gds" => Pane::CellLayout,
            "cell-lef" | "lef" => Pane::CellLef,
            "parasitics" | "lpe" => Pane::Parasitics,
            "details" => Pane::Details,
            _ => return None,
        })
    }
}

/// start layout: details on the left, all views stacked as tabs on the right
pub fn default_dock() -> egui_dock::DockState<Pane> {
    let mut d = egui_dock::DockState::new(VIEW_PANES.to_vec());
    d.main_surface_mut().split_left(egui_dock::NodeIndex::root(), 0.22, vec![Pane::Details]);
    d
}

// ---------- menu commands and keyboard shortcuts ----------
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum OpenKind {
    Liberty,
    Netlist,
    Lef,
    Def,
    Spef,
    Cdl,
    Gds,
    CellLef,
    LayerMap,
}
impl OpenKind {
    fn menu(self) -> &'static str {
        tr(match self {
            OpenKind::Liberty => "Open Liberty (.lib)…",
            OpenKind::Netlist => "Open gate-level netlist (.v)…",
            OpenKind::Lef => "Open LEF…",
            OpenKind::Def => "Open DEF…",
            OpenKind::Spef => "Open SPEF (block parasitics)…",
            OpenKind::Cdl => "Open CDL / SPICE / LPE netlist…",
            OpenKind::Gds => "Open GDSII…",
            OpenKind::CellLef => "Open cell LEF…",
            OpenKind::LayerMap => "Open layer map…",
        })
    }
    fn filter(self) -> (&'static str, &'static [&'static str]) {
        match self {
            OpenKind::Liberty => ("Liberty", &["lib", "liberty", "gz"]),
            OpenKind::Netlist => ("Verilog", &["v", "vg", "sv", "gz"]),
            OpenKind::Lef | OpenKind::CellLef => ("LEF", &["lef", "tlef", "gz"]),
            OpenKind::Def => ("DEF", &["def", "gz"]),
            OpenKind::Spef => ("SPEF", &["spef", "gz"]),
            OpenKind::Cdl => ("CDL / SPICE", &["cdl", "sp", "spi", "spice", "cir", "net", "pex", "dspf", "spf", "gz"]),
            OpenKind::Gds => ("GDSII", &["gds", "gds2", "gdsii", "gz"]),
            OpenKind::LayerMap => (tr("Layer map (.layermap, .lyp)"), &["layermap", "lyp", "map", "txt"]),
        }
    }
    /// several files at once make sense for these
    fn multi(self) -> bool {
        matches!(self, OpenKind::Liberty | OpenKind::Lef)
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cmd {
    OpenAny,
    Open(OpenKind),
    Reload,
    Exit,
    Find,
    CopyName,
    ClearSel,
    Focus(Pane),
    SideBySide,
    Fit,
    ToggleSide,
    PopOut(Pane),
    DockAll,
    ResetLayout,
    Theme(egui::ThemePreference),
    Lang(crate::i18n::Lang),
    Shortcuts,
    About,
}

use egui::{Key, KeyboardShortcut, Modifiers};
const SC_OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
const SC_RELOAD: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
const SC_EXIT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Q);
const SC_FIND: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::F);
const SC_COPY: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::C);
const SC_SIDE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::B);
const SC_TAB1: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num1);
const SC_TAB2: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num2);
const SC_TAB3: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num3);
const SC_TAB4: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num4);
const SC_TAB5: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num5);
const ALL_EXT: [&str; 26] = ["lib", "liberty", "v", "vg", "sv", "lef", "tlef", "def", "spef", "cdl", "sp", "spi", "spice", "cir", "net", "pex", "dspf", "spf", "gds", "gds2", "gdsii", "layermap", "lyp", "map", "txt", "gz"];
const PANE_KEYS: [(Pane, KeyboardShortcut); 5] = [(Pane::Netlist, SC_TAB1), (Pane::BlockLayout, SC_TAB2), (Pane::Schematic, SC_TAB3), (Pane::CellLayout, SC_TAB4), (Pane::CellLef, SC_TAB5)];

/// menu entry with its shortcut shown on the right
fn item(ui: &mut egui::Ui, label: &str, sc: Option<&KeyboardShortcut>) -> bool {
    let mut b = egui::Button::new(label);
    if let Some(s) = sc {
        b = b.shortcut_text(ui.ctx().format_shortcut(s));
    }
    ui.add(b).clicked()
}

/// commands triggered from the keyboard this frame
fn shortcut_cmds(ctx: &egui::Context) -> Vec<Cmd> {
    let mut v = vec![];
    let typing = ctx.egui_wants_keyboard_input();
    ctx.input_mut(|i| {
        for (sc, c) in [(SC_OPEN, Cmd::OpenAny), (SC_RELOAD, Cmd::Reload), (SC_EXIT, Cmd::Exit), (SC_FIND, Cmd::Find), (SC_SIDE, Cmd::ToggleSide)] {
            if i.consume_shortcut(&sc) {
                v.push(c);
            }
        }
        for (p, sc) in PANE_KEYS {
            if i.consume_shortcut(&sc) {
                v.push(Cmd::Focus(p));
            }
        }
        // plain keys and copy only when no text field has the keyboard
        if !typing {
            if i.consume_shortcut(&SC_COPY) {
                v.push(Cmd::CopyName);
            }
            if i.consume_key(Modifiers::NONE, Key::F) {
                v.push(Cmd::Fit);
            }
        }
    });
    v
}

pub struct MenuState {
    pub tab: Tab,
    /// panes on screen right now (the active tab of each dock pane, plus panes in their own windows)
    pub visible: Vec<Pane>,
    /// panes in their own OS windows
    pub popped: Vec<Pane>,
    pub side_by_side: bool,
    pub show_side: bool,
    pub has_lpe: bool,
    pub theme: egui::ThemePreference,
    pub has_sel: bool,
}
impl MenuState {
    pub fn shows(&self, p: Pane) -> bool {
        self.visible.contains(&p)
    }
}

/// small grey heading inside a menu
fn menu_heading(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(tr(text)).small().weak());
}

/// File / Edit / View / Help. Block = the design (netlist and layout), Cell = one library cell (CDL, GDS, LEF).
pub fn menu_bar(ui: &mut egui::Ui, st: &MenuState, cmds: &mut Vec<Cmd>) {
    egui::MenuBar::new().ui(ui, |ui| {
        ui.menu_button(tr("File"), |ui| {
            if item(ui, tr("Open…"), Some(&SC_OPEN)) {
                cmds.push(Cmd::OpenAny);
            }
            ui.separator();
            menu_heading(ui, "Block");
            for k in [OpenKind::Liberty, OpenKind::Netlist, OpenKind::Lef, OpenKind::Def, OpenKind::Spef] {
                if item(ui, k.menu(), None) {
                    cmds.push(Cmd::Open(k));
                }
            }
            ui.separator();
            menu_heading(ui, "Cell");
            for k in [OpenKind::Cdl, OpenKind::Gds, OpenKind::CellLef, OpenKind::LayerMap] {
                if item(ui, k.menu(), None) {
                    cmds.push(Cmd::Open(k));
                }
            }
            ui.separator();
            if item(ui, tr("Reload all files"), Some(&SC_RELOAD)) {
                cmds.push(Cmd::Reload);
            }
            ui.separator();
            if item(ui, tr("Exit"), Some(&SC_EXIT)) {
                cmds.push(Cmd::Exit);
            }
        });
        ui.menu_button(tr("Edit"), |ui| {
            if item(ui, tr("Find…"), Some(&SC_FIND)) {
                cmds.push(Cmd::Find);
            }
            ui.separator();
            if ui.add_enabled(st.has_sel, egui::Button::new(tr("Copy selected name")).shortcut_text(ui.ctx().format_shortcut(&SC_COPY))).clicked() {
                cmds.push(Cmd::CopyName);
            }
            if ui.add_enabled(st.has_sel, egui::Button::new(tr("Clear selection")).shortcut_text("Esc")).clicked() {
                cmds.push(Cmd::ClearSel);
            }
        });
        ui.menu_button(tr("View"), |ui| {
            menu_heading(ui, "Block");
            for (p, label, sc) in [(Pane::Netlist, "Netlist", &SC_TAB1), (Pane::BlockLayout, "Layout", &SC_TAB2)] {
                if ui.add(egui::Button::selectable(st.shows(p), tr(label)).shortcut_text(ui.ctx().format_shortcut(sc))).clicked() {
                    cmds.push(Cmd::Focus(p));
                }
            }
            ui.separator();
            menu_heading(ui, "Cell");
            for (p, label, sc) in [(Pane::Schematic, "Schematic", &SC_TAB3), (Pane::CellLayout, "Layout", &SC_TAB4), (Pane::CellLef, "LEF", &SC_TAB5)] {
                if ui.add(egui::Button::selectable(st.shows(p), tr(label)).shortcut_text(ui.ctx().format_shortcut(sc))).clicked() {
                    cmds.push(Cmd::Focus(p));
                }
            }
            if ui.add_enabled(st.has_lpe, egui::Button::selectable(st.shows(Pane::Parasitics), tr("Parasitics (LPE)"))).clicked() {
                cmds.push(Cmd::Focus(Pane::Parasitics));
            }
            if ui.add(egui::Button::selectable(st.side_by_side, tr("Schematic and layout side by side"))).clicked() {
                cmds.push(Cmd::SideBySide);
            }
            ui.separator();
            ui.menu_button(tr("Own window"), |ui| {
                for p in [Pane::Netlist, Pane::BlockLayout, Pane::Schematic, Pane::CellLayout, Pane::CellLef, Pane::Parasitics, Pane::Details] {
                    if ui.add(egui::Button::selectable(st.popped.contains(&p), p.title())).clicked() {
                        cmds.push(Cmd::PopOut(p));
                    }
                }
            });
            if ui.add_enabled(!st.popped.is_empty(), egui::Button::new(tr("Dock all windows"))).clicked() {
                cmds.push(Cmd::DockAll);
            }
            if ui.button(tr("Reset layout")).clicked() {
                cmds.push(Cmd::ResetLayout);
            }
            ui.separator();
            if ui.add(egui::Button::new(tr("Fit to window")).shortcut_text("F")).clicked() {
                cmds.push(Cmd::Fit);
            }
            if ui.add(egui::Button::selectable(st.show_side, tr("Details panel")).shortcut_text(ui.ctx().format_shortcut(&SC_SIDE))).clicked() {
                cmds.push(Cmd::ToggleSide);
            }
            ui.menu_button(tr("Theme"), |ui| {
                for (p, label) in [(egui::ThemePreference::System, "Follow system"), (egui::ThemePreference::Light, "Light"), (egui::ThemePreference::Dark, "Dark")] {
                    if ui.add(egui::Button::selectable(st.theme == p, tr(label))).clicked() {
                        cmds.push(Cmd::Theme(p));
                    }
                }
            });
            // each language in its own script, and "Language" stays readable in every language
            let lang_label = if crate::i18n::is_en() { "Language".to_string() } else { format!("{} (Language)", tr("Language")) };
            ui.menu_button(lang_label, |ui| {
                for (l, _, name) in crate::i18n::LANGS {
                    if ui.radio(crate::i18n::lang() == l, name).clicked() {
                        cmds.push(Cmd::Lang(l));
                    }
                }
                if let Some(n) = crate::i18n::missing_fonts_note() {
                    ui.label(egui::RichText::new(n).small().weak());
                }
            });
        });
        ui.menu_button(tr("Help"), |ui| {
            if ui.button(tr("Keyboard shortcuts")).clicked() {
                cmds.push(Cmd::Shortcuts);
            }
            if ui.button(tr("About")).clicked() {
                cmds.push(Cmd::About);
            }
        });
    });
}

/// view buttons in two matching groups: the block (netlist, layout) and the cell (schematic, layout, LEF);
/// a button is lit while its pane is on screen and brings the pane forward when clicked
pub fn tool_tabs(ui: &mut egui::Ui, st: &MenuState, cmds: &mut Vec<Cmd>) {
    let group = |ui: &mut egui::Ui, title: &str, tabs: &[(Pane, &str)], cmds: &mut Vec<Cmd>| {
        let fill = ui.visuals().faint_bg_color;
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        egui::Frame::NONE.fill(fill).stroke(stroke).corner_radius(5.0).inner_margin(egui::Margin::symmetric(6, 2)).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(tr(title)).small().weak());
                for (p, label) in tabs {
                    if ui.selectable_label(st.shows(*p), tr(label)).clicked() {
                        cmds.push(Cmd::Focus(*p));
                    }
                }
            });
        });
    };
    group(ui, "BLOCK", &[(Pane::Netlist, "Netlist"), (Pane::BlockLayout, "Layout")], cmds);
    group(ui, "CELL", &[(Pane::Schematic, "Schematic"), (Pane::CellLayout, "Layout"), (Pane::CellLef, "LEF")], cmds);
}
pub fn dialogs(ctx: &egui::Context, show_keys: &mut bool, show_about: &mut bool) {
    egui::Window::new(tr("Keyboard shortcuts")).id(egui::Id::unique("lsv_keys")).open(show_keys).collapsible(false).resizable(false).show(ctx, |ui| {
        let f = |s: &KeyboardShortcut| ctx.format_shortcut(s);
        egui::Grid::new("keys").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
            for (k, what) in [
                (f(&SC_OPEN), "Open files"),
                (f(&SC_RELOAD), "Reload all files"),
                (f(&SC_FIND), "Find cells, nets, transistors"),
                (f(&SC_COPY), "Copy the selected name"),
                ("Esc".to_string(), "Clear the selection"),
                (format!("{} / {}", f(&SC_TAB1), f(&SC_TAB2)), "Block netlist / layout"),
                (format!("{} / {} / {}", f(&SC_TAB3), f(&SC_TAB4), f(&SC_TAB5)), "Cell schematic / layout / LEF"),
                ("Drag a view tab".to_string(), "Drop it on an edge of a view to split, on a tab bar to stack"),
                ("Right click a view tab".to_string(), "Open the view in its own window"),
                ("F".to_string(), "Fit to window"),
                (f(&SC_SIDE), "Show or hide the side panel"),
                ("Wheel".to_string(), "Zoom at the pointer"),
                ("Wheel-button drag".to_string(), "Pan (hold the wheel down and move)"),
                ("Click".to_string(), "Select"),
                ("Right click".to_string(), "Jump to the other views"),
                (f(&SC_EXIT), "Exit"),
            ] {
                ui.monospace(tr(&k));
                ui.label(tr(what));
                ui.end_row();
            }
        });
    });
    egui::Window::new(tr("About")).id(egui::Id::unique("lsv_about")).open(show_about).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.strong(format!("Layout Schematic Visualizer {}", env!("CARGO_PKG_VERSION")));
        ui.label(tr("Block: gate-level netlist from Liberty + Verilog, and LEF/DEF layout on the GPU."));
        ui.label(tr("Cell: CDL transistor schematic and GDS layout with an LVS-style match."));
        ui.label(tr("Everything is cross-probed. Drop files on the window or use File > Open."));
    });
}

/// screenshot of the window as an RGBA PNG
fn save_png(path: &std::path::Path, img: &egui::ColorImage) -> Result<(), String> {
    let (w, h) = (img.size[0] as u32, img.size[1] as u32);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for c in &img.pixels {
        data.extend_from_slice(&c.to_srgba_unmultiplied());
    }
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.write_header().and_then(|mut wr| wr.write_image_data(&data)).map_err(|e| e.to_string())
}

fn sch_colors(vis: &egui::Visuals) -> schematic::Colors {
    let dark = vis.dark_mode;
    schematic::Colors {
        ink: vis.text_color(),
        wire: if dark { egui::Color32::from_rgb(143, 165, 185) } else { egui::Color32::from_rgb(52, 73, 94) },
        muted: vis.weak_text_color(),
        hl: egui::Color32::from_rgb(212, 80, 15),
        sel: if dark { egui::Color32::from_rgb(121, 174, 245) } else { egui::Color32::from_rgb(28, 79, 140) },
        fill: vis.panel_fill,
    }
}

pub struct App {
    files: Files,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    loading: usize,
    status: String,
    tab: Tab,
    // layout
    data: Option<Box<Loaded>>,
    uploaded: bool,
    origin: [f64; 2],
    scale: f64,
    fit_pending: bool,
    show: Show,
    sel: Option<Pick>,
    hl: Arc<Vec<Inst>>,
    hl_version: u64,
    hover: Option<Pick>,
    lmenu: Option<Pick>,
    pending_focus: bool,
    // schematic
    nl: Option<Arc<NlLoaded>>,
    sch: Option<SchLayout>,
    sview: View,
    sfit: bool,
    ssel: Option<Hit>,
    // live cross-probing: the object last clicked in the netlist or the block layout, and the LSV_PROBE test hook
    live: Option<Live>,
    // block parasitics (SPEF): the file, the picked R or C (SPEF net, capacitor?, index), segments of recent nets
    spef: Option<Arc<crate::spef::Spef>>,
    spef_name: String,
    spef_sel: Option<(usize, bool, usize)>,
    spef_segs: Vec<(usize, crate::spef::NetSegs)>,
    spef_focus: bool,
    /// scroll the Details list to the picked R or C once (it was picked elsewhere than in that list)
    spef_scroll: bool,
    spef_hook: Option<String>,
    probe_hook: Option<String>,
    shover: Option<Hit>,
    smenu: Option<Hit>,
    fan_in: usize,
    fan_out: usize,
    // cell (CDL + GDS)
    cell: CellTab,
    cresults: Vec<(u8, String)>,
    // search
    query: String,
    results: Vec<(u8, u32)>,
    exit_at: Option<Instant>,
    // netlist overview on the GPU
    sch_dirty: bool,
    sch_hl: Arc<Vec<Inst>>,
    sch_hl_version: u64,
    sch_hl_for: Option<Option<Hit>>,
    sch_hl_zoom: f32,
    /// draw the whole design (when it is small enough) rather than the cells around the selection
    sch_whole: bool,
    /// opened submodules of the folded view, sheets laid out before, and the layout running in the background
    open: HashSet<u32>,
    fold_cache: fold::Cache,
    fold_busy: bool,
    fold_next: Option<(Option<Hit>, bool)>,
    /// test hooks LSV_OPEN (instance paths) and LSV_FIND (search text), used once the netlist is in
    fold_hooks: Option<(Vec<String>, Option<String>)>,
    /// the start tab was given on the command line: loading does not switch tabs
    tab_pinned: bool,
    /// test hook: save the window to this PNG after the given time, then close
    shot: Option<(PathBuf, Instant)>,
    shot_sent: bool,
    frames: u64,
    frames_reported: Instant,
    // window chrome
    focused_this_frame: bool,
    /// the views and where the user put them; panes in `popped` live in their own OS windows instead
    dock: egui_dock::DockState<Pane>,
    popped: Vec<Pane>,
    /// size of the netlist pane last frame (jumps into the netlist centre on it)
    sch_size: egui::Vec2,
    theme: egui::ThemePreference,
    focus_search: bool,
    show_about: bool,
    show_keys: bool,
}

fn load_layout(files: &Files, tx: &Sender<Msg>) -> Result<Loaded, String> {
    let t0 = Instant::now();
    let mut lib = Lib::default();
    for f in &files.lefs {
        let _ = tx.send(Msg::Status(trf!("Reading {}", f.display())));
        let text = crate::io::read_all(f).map_err(|e| format!("{}: {}", f.display(), e))?;
        parse_lef(&text, &f.display().to_string(), &mut lib)?;
    }
    let def = files.def.as_ref().ok_or(tr("no DEF file"))?;
    let threads = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    let _ = tx.send(Msg::Status(trf!("Reading and parsing {} on {} threads", def.display(), threads)));
    let t1 = Instant::now();
    // read block by block while parsing: the whole DEF text is never in memory
    let (lay, _) = parse_def_file(def, &lib, threads).map_err(|e| format!("{}: {}", def.display(), e))?;
    let t2 = Instant::now();
    let _ = tx.send(Msg::Status(trf!("Building the scene ({} shapes)", lay.rects.len())));
    let scene = Scene::build_lean(&lay, &lib, false);
    // search index on all threads (large designs have millions of names)
    use rayon::prelude::*;
    let mut names: Vec<(String, u8, u32)> = lay.comps.par_iter().enumerate().map(|(i, c)| (c.name.to_lowercase(), 0u8, i as u32)).collect();
    names.par_extend(lay.nets.par_iter().enumerate().map(|(i, n)| (n.name.to_lowercase(), 1u8, i as u32)));
    names.par_sort_by(|a, b| a.0.cmp(&b.0));
    let t3 = Instant::now();
    let log = trf!(
        "{}: {} cells, {} nets, {} shapes | read {:.1} s, parse {:.1} s, scene {:.1} s | warnings {}",
        lay.design, lay.comps.len(), lay.nets.len(), lay.rects.len(), (t1 - t0).as_secs_f64(), (t2 - t1).as_secs_f64(), (t3 - t2).as_secs_f64(), lay.warnings.len()
    );
    Ok(Loaded { lib, lay, scene, names, log })
}

fn load_netlist(files: &Files, tx: &Sender<Msg>) -> Result<NlLoaded, String> {
    let t0 = Instant::now();
    let mut models = HashMap::new();
    for f in &files.libs {
        let _ = tx.send(Msg::Status(trf!("Reading {}", f.display())));
        let text = crate::io::read_all(f).map_err(|e| format!("{}: {}", f.display(), e))?;
        for c in parse_liberty(&text, &f.display().to_string())? {
            models.insert(c.name.clone(), Arc::new(analyze_cell(&c)));
        }
    }
    let nf = files.netlist.as_ref().ok_or(tr("no netlist"))?;
    let _ = tx.send(Msg::Status(trf!("Reading {}", nf.display())));
    let src = crate::io::read_all(nf).map_err(|e| format!("{}: {}", nf.display(), e))?;
    let (order, mods) = parse_verilog(&src, &nf.display().to_string())?;
    drop(src);
    let top = files.top.clone().unwrap_or_else(|| pick_top(&order, &mods)[0].clone());
    let _ = tx.send(Msg::Status(trf!("Elaborating {}", top)));
    let nl = elaborate(&mods, &top, &models)?;
    let log = trf!("{}: {} cells, {} nets, {} ports | netlist {:.1} s | warnings {}", nl.top, nl.cells.len(), nl.nets.len(), nl.ports.len(), t0.elapsed().as_secs_f64(), nl.warnings.len());
    let fold = Fold::build(&nl);
    Ok(NlLoaded { nl, log, fold })
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Files) -> App {
        if let Some(rs) = cc.wgpu_render_state.as_ref() {
            let gpu = Gpu::new(&rs.device, rs.target_format);
            rs.renderer.write().callback_resources.insert(gpu);
            let sch = SchGpu(Gpu::new(&rs.device, rs.target_format));
            rs.renderer.write().callback_resources.insert(sch);
        }
        let (tx, rx) = channel();
        let mut app = App {
            files,
            tx,
            rx,
            loading: 0,
            status: tr("Open files from the File menu, or drop them on the window.").into(),
            tab: Tab::Layout,
            data: None,
            uploaded: false,
            origin: [0.0, 0.0],
            scale: 1.0,
            fit_pending: true,
            show: Show::default(),
            sel: None,
            hl: Arc::new(vec![]),
            hl_version: 0,
            hover: None,
            lmenu: None,
            pending_focus: false,
            nl: None,
            sch: None,
            sview: View { pan: egui::Vec2::ZERO, zoom: 1.0 },
            sfit: true,
            ssel: None,
            live: None,
            spef: None,
            spef_name: String::new(),
            spef_sel: None,
            spef_segs: vec![],
            spef_focus: false,
            spef_scroll: false,
            // test hook: LSV_SPEF_SEL=<net>/R<n> or <net>/C<n> picks that net and that R or C of the SPEF (1-based)
            spef_hook: std::env::var("LSV_SPEF_SEL").ok(),
            probe_hook: std::env::var("LSV_PROBE").ok(),
            shover: None,
            smenu: None,
            fan_in: 2,
            fan_out: 1,
            cell: CellTab::new(CellFiles::default()),
            cresults: vec![],
            query: String::new(),
            results: vec![],
            exit_at: None,
            sch_dirty: false,
            sch_hl: Arc::new(vec![]),
            sch_hl_version: 0,
            sch_hl_for: None,
            sch_hl_zoom: 1.0,
            sch_whole: true,
            open: HashSet::new(),
            fold_cache: fold::Cache::new(),
            fold_busy: false,
            fold_next: None,
            fold_hooks: None,
            tab_pinned: false,
            shot: None,
            shot_sent: false,
            frames: 0,
            frames_reported: Instant::now(),
            focused_this_frame: false,
            dock: default_dock(),
            popped: vec![],
            sch_size: egui::vec2(1000.0, 800.0),
            theme: egui::ThemePreference::System,
            focus_search: false,
            show_about: false,
            show_keys: false,
        };
        app.cell = CellTab::new(CellFiles { cdl: app.files.cdl.clone(), gds: app.files.gds.clone(), layermap: app.files.layermap.clone(), lef: app.files.cell_lef.clone(), lpe: app.files.lpe.clone(), cell: app.files.cell.clone(), libs: app.files.libs.clone() });
        // test hooks: LSV_POP=xtor,gds (or any view names) starts with those views in their own windows,
        // LSV_EXIT_AFTER=N closes the program after N seconds
        if let Ok(p) = std::env::var("LSV_POP") {
            for name in p.split(',') {
                if let Some(pane) = Pane::from_name(name) {
                    app.pop_out(pane);
                }
            }
        }
        app.show_keys = std::env::var("LSV_SHOW_KEYS").is_ok();
        // test hook: LSV_THEME=light|dark picks that theme the way View > Theme does (pictures with a known theme)
        match std::env::var("LSV_THEME").ok().as_deref() {
            Some("light") => app.run_cmd(Cmd::Theme(egui::ThemePreference::Light), &cc.egui_ctx),
            Some("dark") => app.run_cmd(Cmd::Theme(egui::ThemePreference::Dark), &cc.egui_ctx),
            Some(t) => eprintln!("lsv: LSV_THEME={}: expected light or dark", t),
            None => {}
        }
        // test hook: LSV_AROUND=1 starts the netlist view on Around the selection instead of Whole design
        if std::env::var("LSV_AROUND").is_ok() {
            app.sch_whole = false;
        }
        // test hook: LSV_PICK_LANG=<code> picks that language the way View > Language does (and stores it)
        if let Some(l) = std::env::var("LSV_PICK_LANG").ok().and_then(|c| crate::i18n::Lang::from_code(&c)) {
            app.run_cmd(Cmd::Lang(l), &cc.egui_ctx);
        }
        // LSV_SHOT=file.png [LSV_SHOT_AFTER=seconds, default 8]: the program saves its own window and closes
        app.shot = std::env::var("LSV_SHOT").ok().map(|p| {
            let s = std::env::var("LSV_SHOT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()).unwrap_or(8.0);
            (PathBuf::from(p), Instant::now() + std::time::Duration::from_secs_f64(s))
        });
        app.exit_at = std::env::var("LSV_EXIT_AFTER").ok().and_then(|s| s.parse::<f64>().ok()).map(|s| Instant::now() + std::time::Duration::from_secs_f64(s));
        // LSV_OPEN=path,path opens those submodules of the netlist view, LSV_FIND=text searches and jumps to the first hit
        let (hook_open, hook_find) = (std::env::var("LSV_OPEN").ok(), std::env::var("LSV_FIND").ok());
        if hook_open.is_some() || hook_find.is_some() {
            app.fold_hooks = Some((hook_open.map(|v| v.split(',').map(|x| x.to_string()).collect()).unwrap_or_default(), hook_find));
        }
        // test mode watchdog: if the window stops drawing, the timed close above never runs, so a
        // separate thread ends the process 15 s later with exit code 3 (reported as a hang)
        if let Some(t) = app.exit_at {
            std::thread::spawn(move || {
                std::thread::sleep(t.saturating_duration_since(Instant::now()) + std::time::Duration::from_secs(15));
                eprintln!("lsv: test watchdog: no clean close, exiting with code 3");
                std::process::exit(3);
            });
        }
        // the cell pane to show first: the schematic, or the layout / LEF when there is no netlist
        let first_cell_pane = if app.files.cdl.is_some() { Pane::Schematic } else if app.files.gds.is_some() { Pane::CellLayout } else { Pane::CellLef };
        if app.cell.has_input() {
            let lm = app.files.layermap.is_some();
            let lf = app.files.cell_lef.is_some();
            let lp = app.files.lpe.is_some();
            app.cell.start_load(&cc.egui_ctx, true, true, lm, lf, lp);
            if app.files.def.is_none() && app.files.netlist.is_none() || app.files.cell.is_some() {
                app.tab = Tab::Cell;
            }
        }
        if app.files.def.is_some() && !app.files.lefs.is_empty() {
            app.start_layout(&cc.egui_ctx);
        }
        if app.files.netlist.is_some() {
            app.start_netlist(&cc.egui_ctx);
            if app.files.def.is_none() && app.tab != Tab::Cell {
                app.tab = Tab::Schematic;
            }
        }
        if app.files.spef.is_some() {
            app.start_spef(&cc.egui_ctx);
        }
        let mut start = match app.tab {
            Tab::Schematic => Pane::Netlist,
            Tab::Layout => Pane::BlockLayout,
            Tab::Cell => first_cell_pane,
        };
        // a start tab given on the command line wins over the choices above and stays while files load
        if let Some(t) = app.files.tab.clone() {
            let name = t.to_lowercase();
            if name == "cell-both" {
                app.toggle_side_by_side();
                start = Pane::Schematic;
            } else if name == "block-both" || name == "all" {
                app.arrange_views(name == "all");
                start = Pane::Netlist;
            } else if name == "schematic" {
                start = Pane::Netlist;
            } else if let Some(p) = Pane::from_name(&name) {
                start = p;
            }
            if let Some(tab) = start.tab() {
                app.tab = tab;
            }
            app.tab_pinned = true;
        }
        app.focus(start);
        app
    }

    // ---------- dock ----------
    /// bring a pane to the front (re-adding it next to the other views if the user closed it)
    fn focus(&mut self, p: Pane) {
        self.focused_this_frame = true;
        if let Some(t) = p.tab() {
            self.tab = t;
        }
        if self.popped.contains(&p) {
            return;
        }
        if let Some(path) = self.dock.find_tab(&p) {
            let _ = self.dock.set_active_tab(path);
            self.dock.set_focused_node_and_surface(path.node_path());
            return;
        }
        let anchor = VIEW_PANES.iter().filter(|q| **q != p).find_map(|q| self.dock.find_tab(q));
        match anchor {
            Some(path) => {
                if let Ok(leaf) = self.dock.leaf_mut(path.node_path()) {
                    leaf.append_tab(p);
                    let n = leaf.tabs.len() - 1;
                    let _ = leaf.set_active_tab(n);
                }
                self.dock.set_focused_node_and_surface(path.node_path());
            }
            None => self.dock.push_to_first_leaf(p),
        }
    }

    /// panes on screen: the active tab of every dock pane, plus the panes in their own windows
    fn visible_panes(&self) -> Vec<Pane> {
        let mut v: Vec<Pane> = self.dock.iter_leaves().filter_map(|(_, leaf)| leaf.tabs.get(leaf.active.0).copied()).collect();
        v.extend(self.popped.iter().copied());
        v
    }

    /// the cell schematic and cell layout are both on screen in different panes
    fn side_by_side(&self) -> bool {
        let (a, b) = (self.dock.find_tab(&Pane::Schematic), self.dock.find_tab(&Pane::CellLayout));
        let vis = self.visible_panes();
        match (a, b) {
            (Some(a), Some(b)) => a.node_path() != b.node_path() && vis.contains(&Pane::Schematic) && vis.contains(&Pane::CellLayout),
            _ => vis.contains(&Pane::Schematic) && vis.contains(&Pane::CellLayout),
        }
    }

    /// the netlist and the block layout side by side, and with `cells` the cell schematic and the cell layout
    /// side by side below them, so a click in one shows the same object in the others at once
    fn arrange_views(&mut self, cells: bool) {
        let want: &[Pane] = if cells { &[Pane::Netlist, Pane::BlockLayout, Pane::Schematic, Pane::CellLayout] } else { &[Pane::Netlist, Pane::BlockLayout] };
        for &p in want {
            if self.popped.contains(&p) {
                self.dock_back(p);
            }
            if self.dock.find_tab(&p).is_none() {
                self.focus(p);
            }
        }
        for &p in &want[1..] {
            if let Some(t) = self.dock.find_tab(&p) {
                self.dock.remove_tab(t);
            }
        }
        let Some(a) = self.dock.find_tab(&Pane::Netlist) else { return };
        let _ = self.dock.set_active_tab(a);
        let path = a.node_path();
        let mut top = path;
        if cells {
            let [upper, lower] = self.dock.split(path, egui_dock::Split::Below, 0.55, egui_dock::Node::leaf(Pane::Schematic));
            top = egui_dock::NodePath { surface: path.surface, node: upper };
            self.dock.split(egui_dock::NodePath { surface: path.surface, node: lower }, egui_dock::Split::Right, 0.5, egui_dock::Node::leaf(Pane::CellLayout));
        }
        self.dock.split(top, egui_dock::Split::Right, 0.5, egui_dock::Node::leaf(Pane::BlockLayout));
        self.cell.fit();
    }

    /// cell layout to the right of the cell schematic, or back into the schematic's tab stack
    fn toggle_side_by_side(&mut self) {
        for p in [Pane::Schematic, Pane::CellLayout] {
            if self.popped.contains(&p) {
                self.dock_back(p);
            }
            if self.dock.find_tab(&p).is_none() {
                self.focus(p);
            }
        }
        let (Some(a), Some(b)) = (self.dock.find_tab(&Pane::Schematic), self.dock.find_tab(&Pane::CellLayout)) else { return };
        if a.node_path() == b.node_path() {
            self.dock.remove_tab(b);
            if let Some(a) = self.dock.find_tab(&Pane::Schematic) {
                let _ = self.dock.set_active_tab(a);
                self.dock.split(a.node_path(), egui_dock::Split::Right, 0.5, egui_dock::Node::leaf(Pane::CellLayout));
            }
        } else {
            self.dock.move_tab(b, egui_dock::TabDestination::Node(a.node_path(), egui_dock::TabInsert::Append));
            self.focus(Pane::Schematic);
        }
        self.cell.fit();
    }

    /// after a post-layout netlist is read: the parasitics list below the views, the cell layout in front
    fn show_parasitics(&mut self) {
        if self.dock.find_tab(&Pane::Parasitics).is_none() && !self.popped.contains(&Pane::Parasitics) {
            match VIEW_PANES.iter().find_map(|q| self.dock.find_tab(q)) {
                Some(path) => {
                    self.dock.split(path.node_path(), egui_dock::Split::Below, 0.6, egui_dock::Node::leaf(Pane::Parasitics));
                }
                None => self.dock.push_to_first_leaf(Pane::Parasitics),
            }
        }
        self.focus(Pane::CellLayout);
        // the views just got smaller
        self.cell.fit();
    }

    /// move a pane out of the dock into its own OS window
    fn pop_out(&mut self, p: Pane) {
        if self.popped.contains(&p) {
            return;
        }
        if let Some(path) = self.dock.find_tab(&p) {
            self.dock.remove_tab(path);
        }
        self.popped.push(p);
        self.cell.fit();
        self.fit_pending = true;
    }

    /// put a pane from its own window back next to the other views
    fn dock_back(&mut self, p: Pane) {
        self.popped.retain(|q| *q != p);
        self.focus(p);
        self.cell.fit();
        self.fit_pending = true;
    }

    fn start_layout(&mut self, ctx: &egui::Context) {
        let files = self.files.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.loading += 1;
        self.status = tr("Loading the layout…").into();
        std::thread::spawn(move || {
            let r = load_layout(&files, &tx);
            let _ = tx.send(match r {
                Ok(d) => Msg::Done(Box::new(d)),
                Err(e) => Msg::Failed(e),
            });
            ctx.request_repaint();
        });
    }

    fn start_netlist(&mut self, ctx: &egui::Context) {
        let files = self.files.clone();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.loading += 1;
        self.status = tr("Loading the netlist…").into();
        std::thread::spawn(move || {
            let r = load_netlist(&files, &tx);
            let _ = tx.send(match r {
                Ok(d) => Msg::NlDone(Box::new(d)),
                Err(e) => Msg::Failed(e),
            });
            ctx.request_repaint();
        });
    }

    /// read the block parasitics (SPEF) in the background
    fn start_spef(&mut self, ctx: &egui::Context) {
        let Some(p) = self.files.spef.clone() else { return };
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        self.loading += 1;
        self.status = tr("Loading the parasitics (SPEF)…").into();
        std::thread::spawn(move || {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
            let r = crate::io::read_all(&p).map_err(|e| format!("{}: {}", p.display(), e)).and_then(|b| crate::spef::parse_spef(&String::from_utf8_lossy(&b)).map_err(|e| format!("{}: {}", p.display(), e)));
            let _ = tx.send(match r {
                Ok(s) => Msg::SpefDone(Box::new(s), name),
                Err(e) => Msg::Failed(e),
            });
            ctx.request_repaint();
        });
    }

    /// segments of a SPEF net on the block layout, computed once per net (the last few are kept)
    fn spef_net_segs(&mut self, si: usize) -> Option<&crate::spef::NetSegs> {
        let (sp, d) = (self.spef.as_ref()?, self.data.as_ref()?);
        if !self.spef_segs.iter().any(|(k, _)| *k == si) {
            let ns = crate::spef::net_segments(sp.nets.get(si)?, &d.lay, &d.lib);
            if self.spef_segs.len() >= 8 {
                self.spef_segs.remove(0);
            }
            self.spef_segs.push((si, ns));
        }
        self.spef_segs.iter().find(|(k, _)| *k == si).map(|(_, s)| s)
    }

    /// before the block layout is drawn: the LSV_SPEF_SEL test hook, and the segments of the picked net and of the
    /// nets of the picked R or C
    fn spef_prepare(&mut self) {
        if self.spef.is_none() || self.data.is_none() {
            return;
        }
        if let Some(h) = self.spef_hook.take() {
            let sp = self.spef.clone().unwrap();
            let lay_net = |name: &str| self.data.as_ref().and_then(|d| d.lay.net_index.get(name).copied());
            match h.rsplit_once('/').and_then(|(net, e)| Some((net, e.get(..1)?, e.get(1..)?.parse::<usize>().ok()?))) {
                Some((net, kind, n)) if n >= 1 && sp.by_name.contains_key(net) && lay_net(net).is_some() => {
                    let si = sp.by_name[net];
                    let cap = kind.eq_ignore_ascii_case("c");
                    let len = if cap { sp.nets[si].caps.len() } else { sp.nets[si].res.len() };
                    if n <= len {
                        let li = lay_net(net).unwrap();
                        self.select(Some(Pick::Net(li)));
                        self.spef_sel = Some((si, cap, n - 1));
                        self.spef_focus = true;
                        self.spef_scroll = true;
                    } else {
                        eprintln!("lsv: LSV_SPEF_SEL={}: the net has {} of those", h, len);
                    }
                }
                _ => eprintln!("lsv: LSV_SPEF_SEL={}: expected <net>/R<n> or <net>/C<n> of a net in both the SPEF and the DEF", h),
            }
        }
        let mut want = vec![];
        if let (Some(Pick::Net(ni)), Some(sp), Some(d)) = (self.sel, self.spef.as_ref(), self.data.as_ref()) {
            if let Some(&si) = sp.by_name.get(&d.lay.nets[ni as usize].name) {
                want.push(si);
            }
        }
        if let Some((si, cap, k)) = self.spef_sel {
            want.push(si);
            if cap {
                if let Some((oj, _)) = self.spef.as_ref().and_then(|s| s.nets.get(si)).and_then(|n| n.caps.get(k)).and_then(|c| c.other_at) {
                    want.push(oj);
                }
            }
        }
        for si in want {
            self.spef_net_segs(si);
        }
    }

    /// the box (database units) of what the picked R or C is drawn on
    fn spef_sel_box(&self) -> Option<[f64; 4]> {
        let (si, cap, k) = self.spef_sel?;
        let ns = self.spef_segs.iter().find(|(x, _)| *x == si).map(|(_, s)| s)?;
        let seg = if cap { ns.caps.get(k).copied().flatten().map(|(i, _)| i)? } else { *ns.res.get(k)?.as_ref().ok()? };
        let rs = &ns.segs.get(seg)?.rects;
        rs.iter().map(|r| [r[0] as f64, r[1] as f64, r[2] as f64, r[3] as f64]).reduce(|a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])])
    }

    /// draw the picked R or C of the SPEF over the block layout: its metal segment lit, a resistor between the two node
    /// points of an R, a capacitor to a ground symbol for a C to ground, a capacitor to the other net's segment for a
    /// coupling C
    fn paint_spef(&self, painter: &egui::Painter, rect: egui::Rect, dpr: f64) {
        use crate::cellview::{draw_capacitor, draw_ground, draw_resistor, fmt_f, fmt_ohm, SIDE_A, SIDE_B};
        let Some((si, cap, k)) = self.spef_sel else { return };
        let (Some(sp), Some(d)) = (self.spef.as_ref(), self.data.as_ref()) else { return };
        let segs = |n: usize| self.spef_segs.iter().find(|(x, _)| *x == n).map(|(_, s)| s);
        let Some(ns) = segs(si) else { return };
        let sn = &sp.nets[si];
        let c = rect.center();
        let to_s = |p: [f64; 2]| egui::pos2(c.x + ((p[0] - self.origin[0]) * self.scale / dpr) as f32, c.y - ((p[1] - self.origin[1]) * self.scale / dpr) as f32);
        let srect = |r: &[i32; 4]| egui::Rect::from_two_pos(to_s([r[0] as f64, r[1] as f64]), to_s([r[2] as f64, r[3] as f64]));
        let fill = |rs: &[[i32; 4]], col: egui::Color32| {
            for r in rs {
                painter.rect_filled(srect(r), 0.0, col.gamma_multiply(0.45));
                painter.rect_stroke(srect(r), 0.0, egui::Stroke::new(2.0, col), egui::StrokeKind::Middle);
            }
        };
        let dot = |p: egui::Pos2, col: egui::Color32| {
            painter.circle_filled(p, 4.0, col);
            painter.circle_stroke(p, 4.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
        };
        let font = egui::FontId::proportional(12.0);
        let label = |at: egui::Pos2, text: String, col: egui::Color32| {
            let g = painter.layout_no_wrap(text, font.clone(), egui::Color32::BLACK);
            let r = egui::Rect::from_min_size(at, g.size()).expand(2.0);
            painter.rect_filled(r, 3.0, egui::Color32::from_white_alpha(235));
            painter.rect_stroke(r, 3.0, egui::Stroke::new(1.0, col), egui::StrokeKind::Outside);
            painter.galley(at, g, egui::Color32::BLACK);
        };
        if !cap {
            let (Some(r), Some(Ok(i))) = (sn.res.get(k), ns.res.get(k)) else { return };
            let g = &ns.segs[*i];
            fill(&g.rects, SIDE_A);
            let (a, b) = (to_s(g.ends[0]), to_s(g.ends[1]));
            draw_resistor(painter, a, b, egui::Color32::BLACK);
            dot(a, SIDE_A);
            dot(b, SIDE_A);
            label(a + (b - a) * 0.5 + egui::vec2(10.0, 10.0), format!("R {} {} · {} {}", r.name, fmt_ohm(r.ohms), g.layer, sn.name), SIDE_A);
            return;
        }
        let (Some(cc), Some(Some((i, p)))) = (sn.caps.get(k), ns.caps.get(k).copied()) else { return };
        fill(&ns.segs[i].rects, SIDE_A);
        let pa = to_s(p);
        let text = format!("C {} {}", cc.name, fmt_f(cc.farads));
        match cc.other_at {
            Some((oj, on)) => {
                // the other side: the node of the other net, on the segment of that net under it
                let Some(q) = sp.nets.get(oj).and_then(|n| n.nodes.get(on)).and_then(|n| n.xy).map(|q| [q[0] * d.lay.dbu, q[1] * d.lay.dbu]) else { return };
                if let Some(os) = segs(oj) {
                    let tol = 0.5;
                    let holds = |s: &crate::spef::BlockSeg| s.rects.iter().any(|r| q[0] >= r[0] as f64 - tol && q[0] <= r[2] as f64 + tol && q[1] >= r[1] as f64 - tol && q[1] <= r[3] as f64 + tol);
                    if let Some(s) = os.segs.iter().filter(|s| holds(s)).min_by_key(|s| (s.kind != crate::lpe::SegKind::Wire) as u8) {
                        fill(&s.rects, SIDE_B);
                    }
                }
                let pb = to_s(q);
                draw_capacitor(painter, pa, pb, egui::Color32::BLACK);
                dot(pa, SIDE_A);
                dot(pb, SIDE_B);
                label(pa + (pb - pa) * 0.5 + egui::vec2(12.0, 10.0), format!("{} · {} - {}", text, sn.name, sp.nets[oj].name), SIDE_B);
            }
            None => {
                let up = pa.y > c.y;
                let g = pa + egui::vec2(0.0, if up { -34.0 } else { 34.0 });
                draw_capacitor(painter, pa, g, egui::Color32::BLACK);
                draw_ground(painter, g, up, egui::Color32::BLACK);
                dot(pa, SIDE_A);
                label(pa + egui::vec2(14.0, if up { -24.0 } else { 10.0 }), format!("{} · {} to ground", text, sn.name), SIDE_A);
            }
        }
    }

    fn add_files(&mut self, paths: Vec<PathBuf>, ctx: &egui::Context) {
        let (mut lay, mut net) = (false, false);
        let (mut c_cdl, mut c_gds, mut c_map, mut c_lpe) = (false, false, false, false);
        for p in paths {
            let n = p.to_string_lossy().to_lowercase();
            let n = n.strip_suffix(".gz").unwrap_or(&n).to_string();
            if n.ends_with(".cdl") || n.ends_with(".sp") || n.ends_with(".spi") || n.ends_with(".spice") || n.ends_with(".cir") || n.ends_with(".net") || n.ends_with(".pex") || n.contains(".pex.") || n.ends_with(".dspf") || n.ends_with(".spf") {
                // a post-layout netlist goes next to the CDL, not in its place
                let lpe = crate::io::read_all(&p).map(|b| crate::cellview::looks_like_lpe(&String::from_utf8_lossy(&b))).unwrap_or(false);
                if lpe {
                    self.cell.files.lpe = Some(p);
                    c_lpe = true;
                } else {
                    self.cell.files.cdl = Some(p);
                    c_cdl = true;
                }
            } else if n.ends_with(".gds") || n.ends_with(".gds2") || n.ends_with(".gdsii") {
                self.cell.files.gds = Some(p);
                c_gds = true;
            } else if n.ends_with(".layermap") || n.ends_with(".lyp") || n.ends_with(".map") {
                self.cell.files.layermap = Some(p);
                c_map = true;
            } else if n.ends_with(".spef") {
                self.files.spef = Some(p);
                self.start_spef(ctx);
                return;
            } else if n.ends_with(".def") {
                self.files.def = Some(p);
                lay = true;
            } else if n.ends_with(".lef") || n.ends_with(".tlef") {
                if !self.files.lefs.contains(&p) {
                    self.files.lefs.push(p);
                }
                lay = true;
            } else if n.ends_with(".lib") || n.ends_with(".liberty") {
                if !self.files.libs.contains(&p) {
                    self.files.libs.push(p);
                }
                net = true;
            } else if n.ends_with(".v") || n.ends_with(".vg") || n.ends_with(".sv") {
                self.files.netlist = Some(p);
                net = true;
            }
        }
        if lay && self.files.def.is_some() && !self.files.lefs.is_empty() {
            self.start_layout(ctx);
        }
        if net && self.files.netlist.is_some() {
            self.start_netlist(ctx);
        }
        // the cell views read the Liberty functions for the logic check (see libs_pending in update)
        self.cell.files.libs = self.files.libs.clone();
        if c_cdl || c_gds || c_map || c_lpe {
            self.cell.start_load(ctx, c_cdl, c_gds, c_map, false, c_lpe);
            // a dropped GDS alone opens on the layout, anything with a CDL on the schematic
            self.focus(if c_gds && !c_cdl && !c_lpe { Pane::CellLayout } else { Pane::Schematic });
        }
        if !(lay || net || c_cdl || c_gds || c_map || c_lpe) {
            self.status = tr("Those files are not LEF, DEF, Liberty, Verilog, CDL, GDS or a layer map.").into();
        }
    }

    // ---------- menu commands ----------
    /// name of the selected object in the current tab (for Copy)
    fn selection_name(&self) -> Option<String> {
        match self.tab {
            Tab::Layout => {
                let d = self.data.as_ref()?;
                Some(match self.sel? {
                    Pick::Comp(i) => d.lay.comps[i as usize].name.clone(),
                    Pick::Net(i) => d.lay.nets[i as usize].name.clone(),
                    Pick::Pin(i) => d.lay.pins[i as usize].name.clone(),
                })
            }
            Tab::Schematic => {
                let nl = &self.nl.as_ref()?.nl;
                Some(match self.ssel? {
                    Hit::Cell(c) => nl.cells[c as usize].name.clone(),
                    Hit::Net(n) => nl.nets[n as usize].name.clone(),
                    Hit::Group(g) => self.nl.as_ref()?.fold.groups[g as usize].path.clone(),
                    Hit::Bundle(r) => schematic::bundle_nets(self.sch.as_ref()?, r).0,
                })
            }
            Tab::Cell => match self.cell.sel.as_ref()? {
                crate::cellview::CSel::Dev(n) | crate::cellview::CSel::Net(n) => Some(n.clone()),
                crate::cellview::CSel::LNet(id) => self.cell.ex.as_ref().and_then(|e| e.nets[*id].name.clone()),
                crate::cellview::CSel::Finger(_) => None,
            },
        }
    }

    fn run_cmd(&mut self, c: Cmd, ctx: &egui::Context) {
        match c {
            Cmd::OpenAny => {
                if let Some(p) = rfd::FileDialog::new().add_filter(tr("All supported files"), &ALL_EXT).add_filter(tr("All files"), &["*"]).set_title(tr("Open design files")).pick_files() {
                    self.add_files(p, ctx);
                }
            }
            Cmd::Open(OpenKind::CellLef) => {
                let (name, ext) = OpenKind::CellLef.filter();
                if let Some(p) = rfd::FileDialog::new().add_filter(name, ext).add_filter(tr("All files"), &["*"]).set_title(tr("Open cell LEF")).pick_file() {
                    self.cell.files.lef = Some(p);
                    self.cell.start_load(ctx, false, false, false, true, false);
                    self.focus(Pane::CellLef);
                }
            }
            Cmd::Open(k) => {
                let (name, ext) = k.filter();
                let d = rfd::FileDialog::new().add_filter(name, ext).add_filter(tr("All files"), &["*"]).set_title(k.menu().trim_end_matches('…'));
                let picked = if k.multi() { d.pick_files() } else { d.pick_file().map(|p| vec![p]) };
                if let Some(p) = picked {
                    self.add_files(p, ctx);
                }
            }
            Cmd::Reload => {
                let mut any = false;
                if self.files.def.is_some() && !self.files.lefs.is_empty() {
                    self.start_layout(ctx);
                    any = true;
                }
                if self.files.netlist.is_some() {
                    self.start_netlist(ctx);
                    any = true;
                }
                if self.files.spef.is_some() {
                    self.start_spef(ctx);
                    any = true;
                }
                if self.cell.has_input() {
                    let lm = self.cell.files.layermap.is_some();
                    let lf = self.cell.files.lef.is_some();
                    let lp = self.cell.files.lpe.is_some();
                    self.cell.start_load(ctx, true, true, lm, lf, lp);
                    any = true;
                }
                if !any {
                    self.status = tr("Nothing to reload: no files are open.").into();
                }
            }
            Cmd::Exit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Cmd::Find => {
                self.focus_search = true;
            }
            Cmd::CopyName => {
                if let Some(n) = self.selection_name() {
                    self.status = trf!("Copied \"{}\"", n);
                    ctx.copy_text(n);
                }
            }
            Cmd::ClearSel => match self.tab {
                Tab::Layout => self.select(None),
                Tab::Schematic => self.ssel = None,
                Tab::Cell => self.cell.sel = None,
            },
            Cmd::Focus(p) => {
                let before = self.tab;
                if self.popped.contains(&p) {
                    ctx.send_viewport_cmd_to(egui::ViewportId::from_hash_of(("pane", p)), egui::ViewportCommand::Focus);
                }
                self.focus(p);
                if self.tab != before {
                    self.search();
                }
            }
            Cmd::SideBySide => self.toggle_side_by_side(),
            Cmd::Fit => {
                self.fit_pending = true;
                self.sfit = true;
                self.cell.fit();
            }
            Cmd::ToggleSide => {
                if self.popped.contains(&Pane::Details) {
                    self.dock_back(Pane::Details);
                } else if let Some(path) = self.dock.find_tab(&Pane::Details) {
                    self.dock.remove_tab(path);
                } else {
                    self.dock.main_surface_mut().split_left(egui_dock::NodeIndex::root(), 0.22, vec![Pane::Details]);
                }
            }
            Cmd::PopOut(p) => {
                if self.popped.contains(&p) {
                    self.dock_back(p);
                } else {
                    self.pop_out(p);
                }
            }
            Cmd::DockAll => {
                for p in self.popped.clone() {
                    self.dock_back(p);
                }
            }
            Cmd::ResetLayout => {
                self.dock = default_dock();
                self.popped.clear();
                let p = match self.tab {
                    Tab::Schematic => Pane::Netlist,
                    Tab::Layout => Pane::BlockLayout,
                    Tab::Cell => Pane::Schematic,
                };
                self.focus(p);
                self.fit_pending = true;
                self.sfit = true;
                self.cell.fit();
            }
            Cmd::Theme(p) => {
                self.theme = p;
                ctx.set_theme(p);
            }
            Cmd::Lang(l) => {
                crate::i18n::set_lang(l);
                crate::i18n::install_fonts(ctx);
                match crate::i18n::save_lang(l) {
                    Err(e) => self.status = trf!("Could not save the language choice: {}", e),
                    Ok(p) => eprintln!("lsv: language {} stored in {}", l.code(), p.display()),
                }
            }
            Cmd::Shortcuts => self.show_keys = true,
            Cmd::About => self.show_about = true,
        }
    }

    // ---------- layout ----------
    fn fit(&mut self, size_px: egui::Vec2) {
        if let Some(d) = &self.data {
            let die = d.scene.die;
            let (w, h) = ((die[2] - die[0]).max(1) as f64, (die[3] - die[1]).max(1) as f64);
            self.scale = (size_px.x as f64 / w).min(size_px.y as f64 / h) * 0.95;
            self.origin = [(die[0] as f64 + die[2] as f64) / 2.0, (die[1] as f64 + die[3] as f64) / 2.0];
        }
    }

    fn focus_box(&mut self, b: [f64; 4], size_px: egui::Vec2, frac: f64) {
        let (w, h) = ((b[2] - b[0]).max(1.0), (b[3] - b[1]).max(1.0));
        self.scale = (size_px.x as f64 / w).min(size_px.y as f64 / h) * frac;
        self.origin = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
    }

    fn select(&mut self, p: Option<Pick>) {
        self.sel = p;
        let d = match &self.data {
            Some(d) => d,
            None => return,
        };
        let tb = styles_for(&d.scene, [0.0; 3], [0.0; 3], [0.0; 3]);
        let mut v = vec![];
        match p {
            Some(Pick::Comp(ci)) => {
                let c = &d.lay.comps[ci as usize];
                for (ly, _, r) in comp_shapes(&d.lay, &d.lib, c) {
                    if let Some(&li) = d.lay.layer_ids.get(&ly) {
                        v.push(Inst { r: [r[0].round() as i32, r[1].round() as i32, r[2].round() as i32, r[3].round() as i32], slot: slot_of(li, GROUP_SIGNAL) });
                    }
                }
                v.push(Inst { r: [c.x as i32, c.y as i32, (c.x + c.w) as i32, (c.y + c.h) as i32], slot: tb.accent_line });
            }
            Some(Pick::Net(ni)) => {
                let s = &d.scene;
                for &k in &s.net_items[s.net_start[ni as usize] as usize..s.net_start[ni as usize + 1] as usize] {
                    v.push(Inst { r: d.lay.rects.r[k as usize], slot: tb.hl });
                }
            }
            Some(Pick::Pin(pi)) => {
                let r = &d.lay.rects;
                for k in 0..r.len() {
                    if r.kind[k] == crate::K_IOPIN && r.owner[k] == pi as i32 {
                        v.push(Inst { r: r.r[k], slot: tb.hl });
                    }
                }
            }
            None => {}
        }
        self.hl = Arc::new(v);
        self.hl_version += 1;
    }

    fn sel_box(&self) -> Option<[f64; 4]> {
        let d = self.data.as_ref()?;
        match self.sel? {
            Pick::Comp(ci) => {
                let c = &d.lay.comps[ci as usize];
                Some([c.x, c.y, c.x + c.w, c.y + c.h])
            }
            _ => {
                if self.hl.is_empty() {
                    return None;
                }
                let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
                for i in self.hl.iter() {
                    b = [b[0].min(i.r[0] as f64), b[1].min(i.r[1] as f64), b[2].max(i.r[2] as f64), b[3].max(i.r[3] as f64)];
                }
                Some(b)
            }
        }
    }

    fn label(&self, p: Pick) -> String {
        let d = match &self.data {
            Some(d) => d,
            None => return String::new(),
        };
        match p {
            Pick::Comp(ci) => {
                let c = &d.lay.comps[ci as usize];
                format!("{}  ·  {}", c.name, c.model)
            }
            Pick::Net(ni) => d.lay.nets[ni as usize].name.clone(),
            Pick::Pin(pi) => trf!("pin {}", d.lay.pins[pi as usize].name),
        }
    }

    // ---------- schematic ----------
    fn build_schematic(&mut self, focus: &[u32]) {
        // whole design: the folded top sheet with the opened submodules, laid out in the background
        if self.sch_whole && self.fold_fits() {
            let mut at = focus.first().map(|&c| Hit::Cell(c));
            for &c in focus {
                if let Some(g) = self.open_chain(c) {
                    at = Some(Hit::Group(g));
                }
            }
            self.start_fold(at, focus.is_empty());
            return;
        }
        let nl = match &self.nl {
            Some(n) => &n.nl,
            None => return,
        };
        let cells: Vec<u32> = if nl.cells.len() <= FULL_LIMIT && self.sch_whole {
            (0..nl.cells.len() as u32).collect()
        } else {
            let start: Vec<u32> = if focus.is_empty() { vec![0] } else { focus.to_vec() };
            cone_cells(nl, &start, self.fan_in, self.fan_out)
        };
        if cells.is_empty() {
            self.sch = None;
            return;
        }
        let t = Instant::now();
        let lay = schematic::layout(nl, &cells);
        self.status = trf!("Netlist: {} cells of {} drawn ({}) | layout {} ms", cells.len(), nl.cells.len(), if cells.len() == nl.cells.len() { tr("whole design") } else { tr("around the selection") }, t.elapsed().as_millis());
        self.sch = Some(lay);
        self.sch_dirty = true;
        self.sch_hl_for = None;
        self.sfit = true;
    }

    fn sch_has_cell(&self, ci: u32) -> bool {
        self.sch.as_ref().map(|s| s.cell_node.contains_key(&ci)).unwrap_or(false)
    }

    /// select a cell or net in the schematic, rebuilding the view around it when it is not drawn
    fn sch_select(&mut self, h: Hit) {
        let nl = match &self.nl {
            Some(n) => &n.nl,
            None => return,
        };
        match h {
            Hit::Cell(ci) => {
                if !self.sch_has_cell(ci) {
                    self.build_schematic(&[ci]);
                }
            }
            Hit::Net(ni) => {
                let drawn = self.sch.as_ref().map(|s| s.wires.iter().any(|w| w.net == ni) || s.nodes.iter().any(|n| n.ins.iter().chain(n.outs.iter()).any(|x| x.0 == ni))).unwrap_or(false);
                if !drawn && self.sch_whole && self.fold_fits() {
                    // a net inside a closed submodule: open the way to its driver (or first pin)
                    let n = &nl.nets[ni as usize];
                    let first = n.drivers.iter().chain(n.sinks.iter()).find_map(|e| if let End::Cell(c, _) = e { Some(*c) } else { None });
                    let at = first.and_then(|c| self.open_chain(c)).map(Hit::Group).unwrap_or(h);
                    self.start_fold(Some(at), false);
                } else if !drawn {
                    let n = &nl.nets[ni as usize];
                    let cells: Vec<u32> = n.drivers.iter().chain(n.sinks.iter()).filter_map(|e| if let End::Cell(c, _) = e { Some(*c) } else { None }).take(60).collect();
                    let (fi, fo) = (self.fan_in, self.fan_out);
                    self.fan_in = 0;
                    self.fan_out = 0;
                    self.build_schematic(&cells);
                    self.fan_in = fi;
                    self.fan_out = fo;
                }
            }
            // a bundle is always on the sheet it was picked from
            Hit::Bundle(_) => {}
            Hit::Group(g) => {
                let shown = self.sch.as_ref().map(|s| s.group_node.contains_key(&g)).unwrap_or(false);
                if !shown && self.sch_whole && self.fold_fits() {
                    if let Some(n) = self.nl.clone() {
                        let parent = n.fold.groups[g as usize].parent;
                        self.open.extend(n.fold.chain(parent));
                    }
                    self.start_fold(Some(h), false);
                }
            }
        }
        self.ssel = Some(h);
    }

    // ---------- folded netlist view ----------
    /// the top sheet of the folded design is small enough to draw
    fn fold_fits(&self) -> bool {
        self.nl.as_ref().map(|n| n.fold.items(0) <= FULL_LIMIT).unwrap_or(false)
    }

    /// opens the submodules down to a cell, returns the submodule that is too large to open, if one is
    fn open_chain(&mut self, c: u32) -> Option<u32> {
        let n = self.nl.clone()?;
        for g in n.fold.chain(n.fold.cell_group[c as usize]) {
            if n.fold.items(g) > FULL_LIMIT {
                self.status = trf!("{} holds {} cells and submodules directly: too many for one sheet (Around the selection shows them).", n.fold.groups[g as usize].path, n.fold.items(g));
                return Some(g);
            }
            self.open.insert(g);
        }
        None
    }

    /// opens a closed submodule (and the ones above it) or closes an open one with everything below it
    fn toggle_group(&mut self, g: u32) {
        let n = match self.nl.clone() {
            Some(n) => n,
            None => return,
        };
        if !self.open.contains(&g) && n.fold.items(g) > FULL_LIMIT {
            self.status = trf!("{} holds {} cells and submodules directly: too many for one sheet (Around the selection shows them).", n.fold.groups[g as usize].path, n.fold.items(g));
            return;
        }
        n.fold.toggle(&mut self.open, g);
        self.ssel = Some(Hit::Group(g));
        self.start_fold(Some(Hit::Group(g)), false);
    }

    /// lays out the folded view for the current set of opened submodules in a background thread. When it is
    /// back, `focus` is shown (an opened submodule fitted to the view) or the whole sheet fitted (`fit`)
    fn start_fold(&mut self, focus: Option<Hit>, fit: bool) {
        let n = match self.nl.clone() {
            Some(n) => n,
            None => return,
        };
        if self.fold_busy {
            // a layout is running: the newest request is laid out after it
            self.fold_next = Some((focus, fit));
            return;
        }
        self.fold_busy = true;
        self.status = tr("Laying out the netlist view…").into();
        let open = self.open.clone();
        let mut cache = std::mem::take(&mut self.fold_cache);
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let t = Instant::now();
            let lay = fold::view(&n.nl, &n.fold, &open, &mut cache);
            let ms = t.elapsed().as_millis();
            let _ = tx.send(Msg::SchDone(Box::new(FoldDone { lay, cache, focus, fit, ms, nl: n })));
        });
    }

    fn fold_done(&mut self, d: FoldDone, _ctx: &egui::Context) {
        self.fold_busy = false;
        let same = self.nl.as_ref().map(|n| Arc::ptr_eq(n, &d.nl)).unwrap_or(false);
        if same {
            self.fold_cache = d.cache;
        }
        if let Some((focus, fit)) = self.fold_next.take() {
            self.start_fold(focus, fit);
            return;
        }
        if !same || !self.sch_whole {
            return;
        }
        let (cells, total) = (d.lay.cell_node.len(), d.nl.nl.cells.len());
        let closed = d.lay.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Block(_))).count();
        let opened = d.lay.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Frame(_))).count();
        self.status = if d.nl.fold.has_groups() {
            trf!("Netlist: {} cells of {} drawn, {} submodules closed, {} open | layout {} ms", cells, total, closed, opened, d.ms)
        } else {
            trf!("Netlist: {} cells of {} drawn (whole design) | layout {} ms", cells, total, d.ms)
        };
        if self.exit_at.is_some() {
            eprintln!("lsv: folded view: {} | {} nodes, {} wires", self.status, d.lay.nodes.len(), d.lay.wires.len());
        }
        self.sch = Some(d.lay);
        self.sch_dirty = true;
        self.sch_hl_for = None;
        if d.fit {
            self.sfit = true;
        }
        match d.focus {
            Some(Hit::Group(g)) => self.show_group(g),
            Some(h) => self.center_schematic_on(h, self.sch_size),
            None => {}
        }
    }

    /// brings a submodule into view: an open one fitted, a closed box centred (zoomed out to fit if needed)
    fn show_group(&mut self, g: u32) {
        let s = match &self.sch {
            Some(s) => s,
            None => return,
        };
        let n = match s.group_node.get(&g) {
            Some(&i) => &s.nodes[i],
            None => return,
        };
        let size = self.sch_size;
        let fitz = (size.x / n.w).min(size.y / n.h) * 0.9;
        let z = if matches!(n.kind, NodeKind::Frame(_)) { fitz.min(1.6) } else { self.sview.zoom.min(fitz) };
        let c = n.pos + egui::Vec2::new(n.w, n.h) / 2.0;
        self.sview.zoom = z;
        self.sview.pan = c.to_vec2() - size / (2.0 * z);
        self.sfit = false;
    }

    /// test hooks: open the LSV_OPEN submodules, then search for LSV_FIND and jump to the first hit
    fn fold_test_hooks(&mut self) {
        let (open, find) = match self.fold_hooks.take() {
            Some(h) => h,
            None => return,
        };
        let n = match self.nl.clone() {
            Some(n) => n,
            None => return,
        };
        for p in open.iter().filter(|p| !p.is_empty()) {
            match n.fold.groups.iter().position(|g| &g.path == p) {
                Some(g) => self.open.extend(n.fold.chain(g as u32)),
                None => eprintln!("lsv: LSV_OPEN: no submodule {}", p),
            }
        }
        if !self.open.is_empty() {
            self.start_fold(None, true);
        }
        if let Some(q) = find {
            self.query = q.clone();
            self.search();
            match self.results.first().copied() {
                Some((k, id)) => {
                    let h = if k == 0 { Hit::Cell(id) } else { Hit::Net(id) };
                    eprintln!("lsv: LSV_FIND: {} results for {}, jumping to the first", self.results.len(), q);
                    self.sch_select(h);
                    self.center_schematic_on(h, self.sch_size);
                }
                None => eprintln!("lsv: LSV_FIND: no match for {}", q),
            }
        }
    }

    fn center_schematic_on(&mut self, h: Hit, size: egui::Vec2) {
        let s = match &self.sch {
            Some(s) => s,
            None => return,
        };
        let target = match h {
            Hit::Cell(ci) => s.cell_node.get(&ci).map(|&n| s.nodes[n].pos + egui::Vec2::new(s.nodes[n].w, s.nodes[n].h) / 2.0),
            Hit::Net(ni) => schematic::net_point(s, ni),
            Hit::Bundle(r) => s.wires.iter().find(|w| w.bundle && w.net == r).map(|w| w.pts[w.pts.len() / 2]),
            Hit::Group(g) => s.group_node.get(&g).map(|&n| s.nodes[n].pos + egui::Vec2::new(s.nodes[n].w, s.nodes[n].h.min(400.0)) / 2.0),
        };
        if let Some(p) = target {
            if self.sview.zoom < 0.8 {
                self.sview.zoom = 1.2;
            }
            self.sview.pan = p.to_vec2() - size / (2.0 * self.sview.zoom);
            self.sfit = false;
        }
    }

    /// layout -> schematic by name
    /// the netlist object of a layout object, by name (cell instance, net, or the net of a top-level pin)
    fn sch_of(&self, p: Pick) -> Option<Hit> {
        let (d, nl) = (self.data.as_ref()?, &self.nl.as_ref()?.nl);
        match p {
            Pick::Comp(ci) => nl.cell_index.get(&d.lay.comps[ci as usize].name).map(|&c| Hit::Cell(c)),
            Pick::Net(ni) => nl.net_by_name.get(&d.lay.nets[ni as usize].name).map(|&n| Hit::Net(n)),
            Pick::Pin(pi) => d.lay.pins[pi as usize].net.as_ref().and_then(|n| nl.net_by_name.get(n)).map(|&n| Hit::Net(n)),
        }
    }

    /// the layout object of a netlist object, by name (a net by any of its names)
    fn layout_of(&self, h: Hit) -> Option<Pick> {
        let (d, nl) = (self.data.as_ref()?, &self.nl.as_ref()?.nl);
        match h {
            Hit::Cell(ci) => d.lay.comp_index.get(&nl.cells[ci as usize].name).map(|&c| Pick::Comp(c)),
            Hit::Net(ni) => nl.nets[ni as usize].keys.iter().filter(|&&k| !nl.keys[k as usize].starts_with('#')).find_map(|&k| d.lay.net_index.get(&nl.key_display(k)).map(|&n| Pick::Net(n))),
            Hit::Group(_) | Hit::Bundle(_) => None,
        }
    }

    /// live cross-probing: an object clicked in the netlist is picked and framed in the block layout when that is on
    /// screen, and the other way round; a clicked cell instance opens its standard cell in the cell views on screen
    fn live_probe(&mut self) {
        let Some(l) = self.live.take() else { return };
        let vis = self.visible_panes();
        let cell_views = vis.contains(&Pane::Schematic) || vis.contains(&Pane::CellLayout);
        let master = match l {
            Live::Sch(h) => {
                if vis.contains(&Pane::BlockLayout) {
                    if let Some(p) = self.layout_of(h) {
                        self.select(Some(p));
                        self.pending_focus = true;
                    }
                }
                match (h, &self.nl) {
                    (Hit::Cell(ci), Some(n)) => Some(n.nl.cells[ci as usize].typ.clone()),
                    _ => None,
                }
            }
            Live::Lay(p) => {
                if vis.contains(&Pane::Netlist) {
                    if let Some(h) = self.sch_of(p) {
                        self.sch_select(h);
                        self.center_schematic_on(h, self.sch_size);
                    }
                }
                match (p, &self.data) {
                    (Pick::Comp(ci), Some(d)) => Some(d.lay.comps[ci as usize].model.clone()),
                    _ => None,
                }
            }
        };
        if let Some(m) = master.filter(|m| cell_views && self.cell.has_cell(m) && self.cell.cell.as_deref() != Some(m.as_str())) {
            self.cell.request_cell(&m);
        }
    }

    /// test hook LSV_PROBE=<instance or net name>: once everything is read, pick it in the netlist as a click would
    fn probe_test_hook(&mut self) {
        if self.probe_hook.is_none() || self.loading > 0 || self.cell.loading() || self.fold_busy {
            return;
        }
        let Some(n) = self.nl.clone() else { return };
        if self.files.def.is_some() && self.data.is_none() {
            return;
        }
        let q = self.probe_hook.take().unwrap_or_default();
        let h = n.nl.cell_index.get(&q).map(|&c| Hit::Cell(c)).or_else(|| n.nl.net_by_name.get(&q).map(|&x| Hit::Net(x)));
        match h {
            Some(h) => {
                self.sch_select(h);
                self.center_schematic_on(h, self.sch_size);
                self.live = Some(Live::Sch(h));
            }
            None => eprintln!("lsv: LSV_PROBE: no cell or net {}", q),
        }
    }

    fn layout_to_schematic(&mut self, p: Pick, size: egui::Vec2) {
        if self.data.is_none() || self.nl.is_none() {
            self.status = tr("Load a Liberty file and a netlist to see the schematic.").into();
            return;
        }
        let h = self.sch_of(p);
        match h {
            Some(h) => {
                self.sch_select(h);
                self.tab = Tab::Schematic;
                self.center_schematic_on(h, size);
            }
            None => {
                let nm = self.label(p);
                self.status = trf!("{} is not in the netlist (physical-only cells such as fillers and taps have no schematic).", nm);
            }
        }
    }

    /// schematic -> layout by name
    fn schematic_to_layout(&mut self, h: Hit) {
        let nl = match (&self.data, &self.nl) {
            (Some(_), Some(n)) => &n.nl,
            _ => {
                self.status = tr("Load LEF and DEF files to see the layout.").into();
                return;
            }
        };
        let p = self.layout_of(h);
        match p {
            Some(p) => {
                self.select(Some(p));
                self.tab = Tab::Layout;
                self.pending_focus = true;
            }
            None => {
                let nm = match h {
                    Hit::Cell(ci) => nl.cells[ci as usize].name.clone(),
                    Hit::Net(ni) => nl.nets[ni as usize].name.clone(),
                    Hit::Group(g) => self.nl.as_ref().map(|n| n.fold.groups[g as usize].path.clone()).unwrap_or_default(),
                    Hit::Bundle(r) => self.sch.as_ref().map(|s| trf!("bundle {}", schematic::bundle_nets(s, r).0)).unwrap_or_default(),
                };
                self.status = trf!("{} is not in the layout.", nm);
            }
        }
    }

    fn search(&mut self) {
        self.results.clear();
        self.cresults.clear();
        let q = self.query.trim().to_lowercase();
        if q.is_empty() {
            return;
        }
        if self.tab == Tab::Cell {
            self.cresults = self.cell.search(&q);
            self.cresults.truncate(200);
            return;
        }
        if self.tab == Tab::Schematic {
            if let Some(n) = &self.nl {
                let nl = &n.nl;
                for (i, c) in nl.cells.iter().enumerate() {
                    if c.name.to_lowercase().contains(&q) {
                        self.results.push((0, i as u32));
                        if self.results.len() >= 200 {
                            return;
                        }
                    }
                }
                for (i, nt) in nl.nets.iter().enumerate() {
                    if nt.name.to_lowercase().contains(&q) {
                        self.results.push((1, i as u32));
                        if self.results.len() >= 200 {
                            return;
                        }
                    }
                }
            }
            return;
        }
        let d = match &self.data {
            Some(d) => d,
            None => return,
        };
        let start = d.names.partition_point(|n| n.0.as_str() < q.as_str());
        for n in d.names[start..].iter().take_while(|n| n.0.starts_with(&q)).take(200) {
            self.results.push((n.1, n.2));
        }
        if self.results.len() < 200 {
            for n in d.names.iter().filter(|n| !n.0.starts_with(&q) && n.0.contains(&q)).take(200 - self.results.len()) {
                self.results.push((n.1, n.2));
            }
        }
    }
}

struct Cb {
    frame: Frame,
    /// draw with the netlist overview renderer instead of the layout one
    sch: bool,
}

impl egui_wgpu::CallbackTrait for Cb {
    fn prepare(&self, device: &eframe::wgpu::Device, queue: &eframe::wgpu::Queue, _sd: &egui_wgpu::ScreenDescriptor, _enc: &mut eframe::wgpu::CommandEncoder, res: &mut egui_wgpu::CallbackResources) -> Vec<eframe::wgpu::CommandBuffer> {
        let g = if self.sch { res.get_mut::<SchGpu>().map(|g| &mut g.0) } else { res.get_mut::<Gpu>() };
        if let Some(g) = g {
            g.prepare(device, queue, &self.frame);
        }
        vec![]
    }
    fn paint(&self, _info: egui::PaintCallbackInfo, pass: &mut eframe::wgpu::RenderPass<'static>, res: &egui_wgpu::CallbackResources) {
        let g = if self.sch { res.get::<SchGpu>().map(|g| &g.0) } else { res.get::<Gpu>() };
        if let Some(g) = g {
            g.paint(pass, &self.frame);
        }
    }
}

fn c3(c: egui::Color32) -> [f32; 3] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0]
}

impl eframe::App for App {
    fn ui(&mut self, root: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = &root.ctx().clone();
        if self.cell.lpe_new {
            self.cell.lpe_new = false;
            self.show_parasitics();
        }
        if let Some(s) = self.cell.poll() {
            self.status = s;
        }
        if self.exit_at.is_some() {
            // test mode: report drawn frames once a second (to tell a stall from a slow start)
            self.frames += 1;
            let now = Instant::now();
            if now.duration_since(self.frames_reported).as_secs_f32() >= 1.0 {
                eprintln!("lsv: frames {} (tab {:?}, netlist {}, layout {})", self.frames, self.tab, self.nl.is_some(), self.data.is_some());
                self.frames_reported = now;
            }
            // test mode: log user input so a changed view can be told apart from a program fault
            ctx.input(|i| {
                for e in &i.events {
                    match e {
                        egui::Event::PointerButton { button, pressed: true, pos, .. } => eprintln!("lsv: input {:?} button at {:.0},{:.0}", button, pos.x, pos.y),
                        egui::Event::Key { key, pressed: true, modifiers, .. } => eprintln!("lsv: input key {:?} {:?}", key, modifiers),
                        egui::Event::MouseWheel { unit, delta, source, .. } => eprintln!("lsv: input wheel {:?} {:?} {:.1},{:.1}", source, unit, delta.x, delta.y),
                        egui::Event::Zoom(z) => eprintln!("lsv: input pinch {:.3}", z),
                        _ => {}
                    }
                }
            });
        }
        if let Some((path, t)) = self.shot.clone() {
            // only the main window: eframe 0.37 ignores screenshot requests for views in their own windows
            if !self.shot_sent && Instant::now() >= t {
                self.shot_sent = true;
                let c = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    let r = save_png(&path, &img);
                    let note = path.with_extension("txt");
                    let _ = std::fs::write(&note, match r {
                        Ok(()) => format!("saved {}x{}\n", img.size[0], img.size[1]),
                        Err(e) => format!("error {}\n", e),
                    });
                    c.send_viewport_cmd(egui::ViewportCommand::Close);
                    c.request_repaint();
                })));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
        }
        if let Some(t) = self.exit_at {
            if Instant::now() >= t {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(250));
            }
        }
        if self.cell.loading() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Status(s) => self.status = s,
                Msg::Failed(e) => {
                    self.status = trf!("Error: {}", e);
                    self.loading = self.loading.saturating_sub(1);
                }
                Msg::Done(d) => {
                    self.status = d.log.clone();
                    self.data = Some(d);
                    self.uploaded = false;
                    self.fit_pending = true;
                    self.sel = None;
                    self.hl = Arc::new(vec![]);
                    self.hl_version += 1;
                    self.loading = self.loading.saturating_sub(1);
                    if self.tab != Tab::Cell && !self.tab_pinned {
                        self.tab = Tab::Layout;
                    }
                }
                Msg::NlDone(n) => {
                    self.status = n.log.clone();
                    self.nl = Some(Arc::from(n));
                    self.ssel = None;
                    self.open.clear();
                    self.fold_cache.clear();
                    self.loading = self.loading.saturating_sub(1);
                    self.build_schematic(&[]);
                    self.fold_test_hooks();
                    if self.data.is_none() && self.tab != Tab::Cell && !self.tab_pinned {
                        self.tab = Tab::Schematic;
                    }
                }
                Msg::SchDone(d) => self.fold_done(*d, ctx),
                Msg::SpefDone(s, name) => {
                    self.status = trf!("{}: {} nets, {} R, {} C ({} of {} nodes with a location)", name, s.nets.len(), s.nets.iter().map(|n| n.res.len()).sum::<usize>(), s.nets.iter().map(|n| n.caps.len()).sum::<usize>(), s.located, s.node_count);
                    self.spef = Some(Arc::new(*s));
                    self.spef_name = name;
                    self.spef_sel = None;
                    self.spef_segs.clear();
                    self.loading = self.loading.saturating_sub(1);
                }
            }
        }
        if self.loading > 0 || self.fold_busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if !self.uploaded {
            if let (Some(d), Some(rs)) = (self.data.as_mut(), frame.wgpu_render_state()) {
                let t = Instant::now();
                if let Some(g) = rs.renderer.write().callback_resources.get_mut::<Gpu>() {
                    g.upload_layout(&rs.device, &rs.queue, &d.scene, &d.lay);
                }
                d.scene.inst = Vec::new();
                d.scene.order = Vec::new();
                self.uploaded = true;
                self.status = trf!("{} | GPU upload {:.1} s", d.log, t.elapsed().as_secs_f64());
            }
        }
        if self.sch_dirty {
            if let (Some(s), Some(rs)) = (self.sch.as_ref(), frame.wgpu_render_state()) {
                let (inst, ranges) = match self.nl.as_ref() { Some(n) => schematic::gpu_insts(s, &n.nl), None => (vec![], vec![]) };
                if let Some(g) = rs.renderer.write().callback_resources.get_mut::<SchGpu>() {
                    g.0.upload_raw(&rs.device, &inst, ranges);
                }
                self.sch_dirty = false;
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if !dropped.is_empty() {
            self.add_files(dropped, ctx);
        }

        // ---------- menu bar (File / Edit / View / Help) and toolbar ----------
        let mut cmds = shortcut_cmds(ctx);
        let st = MenuState { tab: self.tab, visible: self.visible_panes(), popped: self.popped.clone(), side_by_side: self.side_by_side(), show_side: self.dock.find_tab(&Pane::Details).is_some() || self.popped.contains(&Pane::Details), has_lpe: !self.cell.lpe.is_empty(), theme: self.theme, has_sel: self.selection_name().is_some() };
        egui::Panel::top("menu").show(root, |ui| {
            menu_bar(ui, &st, &mut cmds);
        });
        egui::Panel::top("bar").show(root, |ui| {
            ui.horizontal(|ui| {
                tool_tabs(ui, &st, &mut cmds);
                ui.separator();
                if ui.button(tr("Fit")).on_hover_text(tr("Fit to window (F)")).clicked() {
                    cmds.push(Cmd::Fit);
                }
                let mut te = egui::TextEdit::singleline(&mut self.query).hint_text(tr("Search cells and nets  (Ctrl+F)")).desired_width(260.0);
                te = te.id_salt("search");
                let r = ui.add(te);
                if self.focus_search {
                    r.request_focus();
                    self.focus_search = false;
                }
                if r.changed() {
                    self.search();
                }
            });
        });
        for c in cmds {
            self.run_cmd(c, ctx);
        }
        dialogs(ctx, &mut self.show_keys, &mut self.show_about);
        egui::Panel::bottom("status").show(root, |ui| {
            // a Korean, Japanese or Chinese UI on a computer without such a font
            if let Some(n) = crate::i18n::font_note() {
                ui.colored_label(egui::Color32::from_rgb(201, 42, 42), n);
            }
            if self.tab == Tab::Cell && !self.cell.status.is_empty() {
                ui.label(&self.cell.status);
            } else {
                ui.label(&self.status);
            }
        });
        let mut nav = Nav::default();
        self.cell.side_by_side = st.side_by_side;
        let tab_before = self.tab;
        self.focused_this_frame = false;
        let mut dock = std::mem::replace(&mut self.dock, egui_dock::DockState::new(vec![]));
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(root, |ui| {
            let style = egui_dock::Style::from_egui(ui.style());
            let mut viewer = Viewer { app: &mut *self, nav: &mut nav };
            egui_dock::DockArea::new(&mut dock).id(egui::Id::unique("lsv_dock")).style(style).show_inside(ui, &mut viewer);
        });
        self.dock = dock;
        // views in their own OS windows; closing the window puts the view back into the dock
        for p in self.popped.clone() {
            let mut back = false;
            ctx.show_viewport_immediate(egui::ViewportId::from_hash_of(("pane", p)), egui::ViewportBuilder::default().with_title(format!("{}  ·  Layout Schematic Visualizer", p.title())).with_inner_size([900.0, 700.0]), |vui, _| {
                let vctx = vui.ctx().clone();
                self.pane_ui(vui, p, &mut nav);
                if vctx.input(|i| i.viewport().close_requested()) {
                    back = true;
                }
            });
            if back {
                self.dock_back(p);
            }
        }
        self.apply_nav(nav, ctx);
        // Liberty files added while the cell views were reading (or after): read them once the views are idle
        if self.cell.libs_pending() && self.cell.has_input() {
            self.cell.start_load(ctx, false, false, false, false, false);
        }
        self.probe_test_hook();
        if self.live.is_some() {
            self.live_probe();
            ctx.request_repaint();
        }
        for a in std::mem::take(&mut self.cell.ask) {
            match a {
                CellAsk::Focus(v) => self.focus(Pane::of_cell(v)),
                CellAsk::SideBySide => self.toggle_side_by_side(),
            }
        }
        // code that moved to another view family this frame brings that view forward; otherwise the view
        // the user last clicked into decides which family the search and the details pane follow
        if self.tab != tab_before {
            if !self.focused_this_frame {
                let p = match self.tab {
                    Tab::Schematic => Pane::Netlist,
                    Tab::Layout => Pane::BlockLayout,
                    Tab::Cell => Pane::Schematic,
                };
                self.focus(p);
            }
            self.search();
        } else if let Some((_, p)) = self.dock.find_active_focused() {
            if let Some(t) = p.tab() {
                if t != self.tab {
                    self.tab = t;
                    self.search();
                }
            }
        }
    }
}

/// navigation the details pane and the views ask for, done after the dock is drawn
/// an object clicked in one block view, to be picked in the other views on screen
#[derive(Clone, Copy)]
enum Live {
    Sch(Hit),
    Lay(Pick),
}

#[derive(Default)]
struct Nav {
    cgo: Option<(u8, String)>,
    to_cell: Option<String>,
    jump: Option<Pick>,
    sjump: Option<Hit>,
    to_layout: Option<Hit>,
    to_sch: Option<Pick>,
    rebuild: bool,
    cmds: Vec<Cmd>,
    /// open or close this submodule of the netlist view
    toggle: Option<u32>,
    /// an R or C of the SPEF picked in the Details panel (SPEF net, capacitor?, index)
    spef_pick: Option<(usize, bool, usize)>,
}

struct Viewer<'a> {
    app: &'a mut App,
    nav: &'a mut Nav,
}

impl egui_dock::TabViewer for Viewer<'_> {
    type Tab = Pane;
    fn title(&mut self, tab: &mut Pane) -> egui::WidgetText {
        tab.title().into()
    }
    fn id(&mut self, tab: &mut Pane) -> egui::Id {
        egui::Id::unique(("pane", *tab))
    }
    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Pane) {
        self.app.pane_ui(ui, *tab, self.nav);
    }
    fn context_menu(&mut self, ui: &mut egui::Ui, tab: &mut Pane, _path: egui_dock::NodePath) {
        if ui.button(tr("Open in its own window")).clicked() {
            self.nav.cmds.push(Cmd::PopOut(*tab));
            ui.close();
        }
        if matches!(*tab, Pane::Schematic | Pane::CellLayout) && ui.button(tr("Schematic and layout side by side")).clicked() {
            self.nav.cmds.push(Cmd::SideBySide);
            ui.close();
        }
    }
    fn scroll_bars(&self, _tab: &Pane) -> [bool; 2] {
        [false, false]
    }
}

impl App {
    /// one view, drawn into a dock pane or into its own window
    fn pane_ui(&mut self, ui: &mut egui::Ui, p: Pane, nav: &mut Nav) {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        if !matches!(p, Pane::Details | Pane::Parasitics) {
            ui.painter().rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
        }
        let col = sch_colors(ui.visuals());
        match p {
            Pane::Netlist => {
                self.sch_size = rect.size();
                self.schematic_canvas(ui, rect, &ctx);
            }
            Pane::BlockLayout => self.layout_canvas(ui, rect, &ctx, None),
            Pane::Schematic => self.cell.schematic_pane(ui, rect, &ctx, &col),
            Pane::CellLayout => self.cell.layout_pane(ui, rect, &ctx, &col),
            Pane::CellLef => {
                let block = self.data.as_ref().map(|d| &d.lib);
                self.cell.lef_pane(ui, rect, &ctx, &col, block);
            }
            Pane::Parasitics => {
                egui::Frame::NONE.inner_margin(egui::Margin::same(6)).show(ui, |ui| self.cell.parasitics_pane(ui));
            }
            Pane::Details => {
                egui::ScrollArea::vertical().id_salt("details").show(ui, |ui| self.details_ui(ui, nav));
            }
        }
    }

    fn apply_nav(&mut self, nav: Nav, ctx: &egui::Context) {
        if nav.rebuild {
            let f: Vec<u32> = match self.ssel {
                Some(Hit::Cell(c)) => vec![c],
                _ => vec![],
            };
            self.build_schematic(&f);
        }
        if let Some(g) = nav.toggle {
            self.toggle_group(g);
        }
        if let Some(h) = nav.to_layout {
            self.schematic_to_layout(h);
        }
        if let Some(r) = nav.cgo {
            self.cell.go(&r);
        }
        if let Some(c) = nav.to_cell {
            self.cell.request_cell(&c);
            self.focus(Pane::Schematic);
        }
        if let Some(p) = nav.to_sch {
            self.layout_to_schematic(p, self.sch_size);
        }
        if let Some(h) = nav.sjump {
            self.sch_select(h);
            self.center_schematic_on(h, self.sch_size);
        }
        if let Some(p) = nav.jump {
            self.select(Some(p));
            self.pending_focus = true;
        }
        if let Some(s) = nav.spef_pick {
            self.spef_sel = if self.spef_sel == Some(s) { None } else { Some(s) };
            self.spef_focus = self.spef_sel.is_some();
        }
        for c in nav.cmds {
            self.run_cmd(c, ctx);
        }
    }
    /// the details pane: search results, then what the current view family shows about its selection
    fn details_ui(&mut self, ui: &mut egui::Ui, nav: &mut Nav) {
        if self.tab == Tab::Cell && !self.cresults.is_empty() {
            ui.heading(tr("Search"));
            for r in self.cresults.iter().take(80) {
                let k = tr(["cell ", "xtor ", "net  "][r.0 as usize]);
                if ui.link(format!("{} {}", k, r.1)).clicked() {
                    nav.cgo = Some(r.clone());
                }
            }
            ui.separator();
        }
        if self.tab != Tab::Cell && !self.results.is_empty() {
            ui.heading(tr("Search"));
            if self.tab == Tab::Schematic {
                if let Some(n) = &self.nl {
                    for &(k, id) in self.results.iter().take(80) {
                        let (txt, h) = if k == 0 { (trf!("cell  {}  ·  {}", n.nl.cells[id as usize].name, n.nl.cells[id as usize].typ), Hit::Cell(id)) } else { (trf!("net   {}", n.nl.nets[id as usize].name), Hit::Net(id)) };
                        if ui.link(txt).clicked() {
                            nav.sjump = Some(h);
                        }
                    }
                }
            } else if let Some(d) = &self.data {
                for &(k, id) in self.results.iter().take(80) {
                    let (txt, p) = if k == 0 { (trf!("cell  {}  ·  {}", d.lay.comps[id as usize].name, d.lay.comps[id as usize].model), Pick::Comp(id)) } else { (trf!("net   {}", d.lay.nets[id as usize].name), Pick::Net(id)) };
                    if ui.link(txt).clicked() {
                        nav.jump = Some(p);
                    }
                }
            }
            ui.separator();
        }
        if self.tab == Tab::Cell {
            self.cell.side_panel(ui);
        } else if self.tab == Tab::Schematic {
            ui.heading(tr("Netlist"));
            match &self.nl {
                None => {
                    ui.weak(tr("Open a Liberty file and a gate-level netlist."));
                }
                Some(n) => {
                    let big = n.fold.items(0) > FULL_LIMIT;
                    if big {
                        ui.label(trf!("{} cells: too many for one sheet, so the view shows the cells around the selection.", n.nl.cells.len()));
                    } else {
                        ui.horizontal(|ui| {
                            if ui.radio(self.sch_whole, tr("Whole design")).clicked() && !self.sch_whole {
                                self.sch_whole = true;
                                nav.rebuild = true;
                            }
                            if ui.radio(!self.sch_whole, tr("Around the selection")).clicked() && self.sch_whole {
                                self.sch_whole = false;
                                nav.rebuild = true;
                            }
                        });
                    }
                    if big || !self.sch_whole {
                        ui.horizontal(|ui| {
                            ui.label(tr("fan-in"));
                            ui.add(egui::DragValue::new(&mut self.fan_in).range(0..=20));
                            ui.label(tr("fan-out"));
                            ui.add(egui::DragValue::new(&mut self.fan_out).range(0..=20));
                            if ui.button(tr("Redraw")).clicked() {
                                nav.rebuild = true;
                            }
                        });
                    }
                }
            }
            ui.separator();
            ui.heading(tr("Selection"));
            if let (Some(n), Some(Hit::Group(g))) = (&self.nl, self.ssel) {
                // a submodule box or frame
                let gr = &n.fold.groups[g as usize];
                ui.strong(&gr.path);
                if gr.module.is_empty() {
                    ui.label(tr("group taken from instance names"));
                } else {
                    ui.label(trf!("module {}", gr.module));
                }
                ui.label(trf!("{} cells, {} submodules, {} pins", gr.total, gr.children.len(), gr.pins.len()));
                let open = self.open.contains(&g);
                if ui.button(if open { tr("Collapse") } else { tr("Expand") }).clicked() {
                    nav.toggle = Some(g);
                }
                for p in gr.pins.iter().take(200) {
                    ui.horizontal(|ui| {
                        ui.monospace(if p.out { "out" } else { "in " });
                        let net = &n.nl.nets[p.net as usize].name;
                        let txt = if &p.name == net { p.name.clone() } else { format!("{}  ({})", p.name, net) };
                        if ui.link(txt).clicked() {
                            nav.sjump = Some(Hit::Net(p.net));
                        }
                    });
                }
            } else if let (Some(n), Some(h)) = (&self.nl, self.ssel) {
                let nl = &n.nl;
                match h {
                    Hit::Group(_) => {}
                    Hit::Bundle(r) => {
                        // a bundle: its member nets, each one a link
                        let (label, nets) = self.sch.as_ref().map(|s| schematic::bundle_nets(s, r)).unwrap_or_default();
                        ui.strong(&label);
                        ui.label(trf!("bundle of {} nets", nets.len()));
                        for &m in nets.iter().take(300) {
                            if ui.link(&nl.nets[m as usize].name).clicked() {
                                nav.sjump = Some(Hit::Net(m));
                            }
                        }
                    }
                    Hit::Cell(ci) => {
                        let c = &nl.cells[ci as usize];
                        ui.strong(&c.name);
                        ui.label(format!("{} · {}{}", c.typ, c.model.kind_name(), if c.model.kind_text.is_empty() { String::new() } else { format!(" {}", c.model.kind_text) }));
                        let g = n.fold.cell_group[ci as usize];
                        if g != 0 {
                            let gp = &n.fold.groups[g as usize].path;
                            ui.weak(if n.fold.linked[ci as usize] { trf!("in {} (placed by its connections)", gp) } else { trf!("in {}", gp) });
                        }
                        for (o, g) in &c.model.out_gates {
                            ui.monospace(format!("{} = {}", o, describe_gate(g)));
                        }
                        for (o, f) in &c.model.funcs {
                            ui.small(format!("Liberty {}: {}", o, f));
                        }
                        for p in &c.pins {
                            ui.horizontal(|ui| {
                                ui.monospace(format!("{:5} {:3}", p.pin, &p.dir[..p.dir.len().min(3)]));
                                if ui.link(&nl.nets[p.net as usize].name).clicked() {
                                    nav.sjump = Some(Hit::Net(p.net));
                                }
                            });
                        }
                    }
                    Hit::Net(ni) => {
                        let nt = &nl.nets[ni as usize];
                        ui.strong(&nt.name);
                        ui.label(trf!("{} drivers · {} sinks", nt.drivers.len(), nt.sinks.len()));
                        for e in nt.drivers.iter().chain(nt.sinks.iter()).take(200) {
                            ui.horizontal(|ui| match e {
                                End::Cell(c, pi) => {
                                    if ui.link(&nl.cells[*c as usize].name).clicked() {
                                        nav.sjump = Some(Hit::Cell(*c));
                                    }
                                    ui.monospace(&nl.cells[*c as usize].pins[*pi as usize].pin);
                                }
                                End::Port(p) => {
                                    ui.monospace(trf!("port {} ({})", nl.ports[*p as usize].name, nl.ports[*p as usize].dir));
                                }
                                End::Const(v) => {
                                    ui.monospace(trf!("constant {}", *v as char));
                                }
                            });
                        }
                    }
                }
                if self.data.is_some() && ui.button(tr("Show in layout")).clicked() {
                    nav.to_layout = Some(h);
                }
                if let Hit::Cell(ci) = h {
                    let typ = &nl.cells[ci as usize].typ;
                    if self.cell.has_cell(typ) && ui.button(trf!("Show {} transistors", typ)).clicked() {
                        nav.to_cell = Some(typ.clone());
                    }
                }
            } else {
                ui.weak(tr("Click a gate or a wire."));
            }
        } else {
            ui.heading(tr("Show"));
            ui.checkbox(&mut self.show.cells, tr("Cells"));
            ui.checkbox(&mut self.show.groups[GROUP_SIGNAL], tr("Signal routing"));
            ui.checkbox(&mut self.show.groups[GROUP_POWER], tr("Power routing"));
            ui.checkbox(&mut self.show.groups[GROUP_PIN], tr("Top-level pins"));
            ui.separator();
            ui.heading(tr("Layers"));
            if let Some(d) = self.data.as_mut() {
                for st in d.scene.styles.iter_mut().filter(|s| s.count > 0) {
                    ui.horizontal(|ui| {
                        let col = egui::Color32::from_rgb((st.color[0] * 255.0) as u8, (st.color[1] * 255.0) as u8, (st.color[2] * 255.0) as u8);
                        let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
                        ui.painter().rect_filled(r, 2.0, col);
                        ui.checkbox(&mut st.visible, format!("{}  ({})", st.name, st.count));
                    });
                }
            }
            ui.separator();
            ui.heading(tr("Selection"));
            if let (Some(d), Some(p)) = (&self.data, self.sel) {
                let um = |v: f64| v / d.lay.dbu;
                match p {
                    Pick::Comp(ci) => {
                        let c = &d.lay.comps[ci as usize];
                        ui.strong(&c.name);
                        ui.label(format!("{}  ({})", c.model, d.lib.macros.get(&c.model).map(|m| m.class.clone()).unwrap_or_default()));
                        ui.label(trf!("location {:.3}, {:.3} µm · {} · {}", um(c.x), um(c.y), c.orient, c.status));
                        for (pin, ni) in &d.scene.comp_net_pins[ci as usize] {
                            ui.horizontal(|ui| {
                                ui.monospace(format!("{:6}", pin));
                                if ui.link(&d.lay.nets[*ni as usize].name).clicked() {
                                    nav.jump = Some(Pick::Net(*ni));
                                }
                            });
                        }
                    }
                    Pick::Net(ni) => {
                        let n = &d.lay.nets[ni as usize];
                        ui.strong(&n.name);
                        let shapes = d.scene.net_start[ni as usize + 1] - d.scene.net_start[ni as usize];
                        ui.label(trf!("use {} · {} connections · {} shapes", if n.usage.is_empty() { "-" } else { &n.usage }, n.conns.len(), shapes));
                        // the net's parasitics from the SPEF: each R and C, a click draws it on its metal segment
                        if let Some(sp) = self.spef.as_ref() {
                            match sp.by_name.get(&n.name).copied() {
                                Some(si) => {
                                    let sn = &sp.nets[si];
                                    let segs = self.spef_segs.iter().find(|(k, _)| *k == si).map(|(_, s)| s);
                                    let (scroll, mut scrolled) = (self.spef_scroll, false);
                                    let mut row = |ui: &mut egui::Ui, on: bool, txt: String| -> bool {
                                        let r = ui.selectable_label(on, egui::RichText::new(txt).monospace().small());
                                        if on && scroll {
                                            r.scroll_to_me(Some(egui::Align::Center));
                                            scrolled = true;
                                        }
                                        r.clicked()
                                    };
                                    egui::CollapsingHeader::new(trf!("Parasitics ({}): {} R, {} C, total {}", self.spef_name, sn.res.len(), sn.caps.len(), crate::cellview::fmt_f(sn.total_f))).id_salt("spef_net").default_open(true).show(ui, |ui| {
                                        egui::ScrollArea::vertical().id_salt("spef_rows").max_height(260.0).show(ui, |ui| {
                                            for (k, r) in sn.res.iter().enumerate() {
                                                let seg = segs.and_then(|s| s.res.get(k)).and_then(|x| x.as_ref().ok()).and_then(|&i| segs.map(|s| &s.segs[i]));
                                                let where_ = seg.map(|g| {
                                                    let len = ((g.ends[0][0] - g.ends[1][0]).powi(2) + (g.ends[0][1] - g.ends[1][1]).powi(2)).sqrt() / d.lay.dbu;
                                                    format!("{:?} {} {:.3} µm", g.kind, g.layer, len)
                                                }).unwrap_or_default();
                                                let txt = format!("R {:<5} {:>11}  {}", r.name, crate::cellview::fmt_ohm(r.ohms), where_);
                                                if row(ui, self.spef_sel == Some((si, false, k)), txt) {
                                                    nav.spef_pick = Some((si, false, k));
                                                }
                                            }
                                            for (k, c) in sn.caps.iter().enumerate() {
                                                let to = c.other.clone().unwrap_or_else(|| tr("ground").to_string());
                                                let txt = format!("C {:<5} {:>11}  {} - {}", c.name, crate::cellview::fmt_f(c.farads), sn.nodes[c.a].name, to);
                                                if row(ui, self.spef_sel == Some((si, true, k)), txt) {
                                                    nav.spef_pick = Some((si, true, k));
                                                }
                                            }
                                        });
                                    });
                                    if scrolled {
                                        self.spef_scroll = false;
                                    }
                                }
                                None => {
                                    ui.weak(trf!("{} has no net {}", self.spef_name, n.name));
                                }
                            }
                        }
                        for (c, pin) in n.conns.iter().take(200) {
                            ui.horizontal(|ui| match d.lay.comp_index.get(c) {
                                Some(&ci) => {
                                    if ui.link(c).clicked() {
                                        nav.jump = Some(Pick::Comp(ci));
                                    }
                                    ui.monospace(pin);
                                }
                                None => {
                                    ui.monospace(format!("{} {}", c, pin));
                                }
                            });
                        }
                    }
                    Pick::Pin(pi) => {
                        let p = &d.lay.pins[pi as usize];
                        ui.strong(&p.name);
                        ui.label(trf!("top-level pin · {} · net {}", p.dir, p.net.clone().unwrap_or_default()));
                    }
                }
                if self.nl.is_some() && ui.button(tr("Show in netlist")).clicked() {
                    nav.to_sch = Some(p);
                }
                if let Pick::Comp(ci) = p {
                    let model = &d.lay.comps[ci as usize].model;
                    if self.cell.has_cell(model) && ui.button(trf!("Show {} transistors", model)).clicked() {
                        nav.to_cell = Some(model.clone());
                    }
                }
            } else {
                ui.weak(tr("Click a cell or a wire."));
            }
        }
    }

}

impl App {
    fn layout_canvas(&mut self, ui: &mut egui::Ui, rect: egui::Rect, ctx: &egui::Context, jump: Option<Pick>) {
        let dpr = ctx.pixels_per_point() as f64;
        let size_px = rect.size() * dpr as f32;
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        if self.data.is_none() {
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tr("Drop LEF and DEF files here"), egui::FontId::proportional(18.0), ui.visuals().weak_text_color());
            return;
        }
        if self.fit_pending {
            self.fit(size_px);
            self.fit_pending = false;
        }
        if let Some(p) = jump {
            self.select(Some(p));
            self.pending_focus = true;
        }
        self.spef_prepare();
        if self.spef_focus {
            if let Some(b) = self.spef_sel_box() {
                self.spef_focus = false;
                self.pending_focus = false;
                self.focus_box(b, size_px, 0.5);
            }
        }
        if self.pending_focus {
            self.pending_focus = false;
            if let Some(b) = self.sel_box() {
                let frac = if matches!(self.sel, Some(Pick::Comp(_))) { 0.12 } else { 0.85 };
                self.focus_box(b, size_px, frac);
            }
        }
        let to_world = |app: &App, pos: egui::Pos2| -> [f64; 2] {
            let dx = (pos.x - rect.center().x) as f64 * dpr / app.scale;
            let dy = (pos.y - rect.center().y) as f64 * dpr / app.scale;
            [app.origin[0] + dx, app.origin[1] - dy]
        };
        if resp.dragged_by(egui::PointerButton::Middle) {
            let d = resp.drag_delta();
            self.origin[0] -= d.x as f64 * dpr / self.scale;
            self.origin[1] += d.y as f64 * dpr / self.scale;
        }
        if let Some(pos) = resp.hover_pos() {
            let (zf, pan) = crate::nav::wheel(ctx);
            if zf != 1.0 {
                let before = to_world(self, pos);
                self.scale *= zf as f64;
                let after = to_world(self, pos);
                self.origin[0] += before[0] - after[0];
                self.origin[1] += before[1] - after[1];
            }
            self.origin[0] -= pan.x as f64 * dpr / self.scale;
            self.origin[1] += pan.y as f64 * dpr / self.scale;
            let w = to_world(self, pos);
            let tol = (3.0 * dpr / self.scale) as i64;
            let d = self.data.as_ref().unwrap();
            self.hover = d.scene.pick(&d.lay, w[0] as i64, w[1] as i64, tol, &self.show);
            if resp.clicked() {
                let h = self.hover;
                self.select(h);
                self.live = h.map(Live::Lay);
            }
            if resp.secondary_clicked() {
                self.lmenu = self.hover;
                if let Some(h) = self.hover {
                    self.select(Some(h));
                }
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F)) && !ctx.egui_wants_keyboard_input() {
            self.fit(size_px);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.select(None);
        }
        let d = self.data.as_ref().unwrap();
        let vis = ui.visuals();
        let bg = c3(vis.extreme_bg_color);
        let fg = c3(vis.text_color());
        let tb = styles_for(&d.scene, [0.83, 0.31, 0.06], bg, fg);
        let mut visible = vec![false; d.scene.slot_range.len()];
        visible[0] = self.show.cells;
        for (li, st) in d.scene.styles.iter().enumerate() {
            for g in 0..GROUPS {
                visible[slot_of(li as u8, g) as usize] = st.visible && self.show.groups[g];
            }
        }
        let origin_i = [self.origin[0].round() as i32, self.origin[1].round() as i32];
        let mut extra = vec![Inst { r: d.scene.die, slot: tb.die }];
        if !self.hl.is_empty() {
            let big = [i32::MIN / 4, i32::MIN / 4, i32::MAX / 4, i32::MAX / 4];
            extra.push(Inst { r: big, slot: tb.veil });
        }
        let fr = Frame { cam: Cam { origin: origin_i, size: [size_px.x, size_px.y], scale: self.scale as f32, dpr: dpr as f32, pad: [0.0; 2] }, styles: tb.styles, visible, extra, veil_from: 1, hl: self.hl.clone(), hl_version: self.hl_version };
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, Cb { frame: fr, sch: false }));
        self.paint_spef(&ui.painter_at(rect), rect, dpr);
        let menu_target = self.lmenu;
        let has_nl = self.nl.is_some();
        let mut go: Option<Pick> = None;
        let mut go_cell: Option<String> = None;
        let label = menu_target.map(|p| self.label(p)).unwrap_or_default();
        let master = match menu_target {
            Some(Pick::Comp(ci)) => Some(d.lay.comps[ci as usize].model.clone()).filter(|m| self.cell.has_cell(m)),
            _ => None,
        };
        resp.context_menu(|ui| {
            if let Some(p) = menu_target {
                ui.label(egui::RichText::new(&label).monospace().small());
                if ui.add_enabled(has_nl, egui::Button::new(tr("Show in netlist"))).clicked() {
                    go = Some(p);
                    ui.close();
                }
                if let Some(m) = &master {
                    if ui.button(trf!("Show {} transistors", m)).clicked() {
                        go_cell = Some(m.clone());
                        ui.close();
                    }
                }
            } else {
                ui.label(tr("Nothing here"));
            }
        });
        if let Some(h) = self.hover {
            let txt = self.label(h);
            resp.on_hover_text_at_pointer(txt);
        }
        if let Some(p) = go {
            self.layout_to_schematic(p, rect.size());
        }
        if let Some(m) = go_cell {
            self.cell.request_cell(&m);
            self.focus(Pane::Schematic);
        }
    }

    fn schematic_canvas(&mut self, ui: &mut egui::Ui, rect: egui::Rect, ctx: &egui::Context) {
        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let mut toggle: Option<u32> = None;
        let sch = match &self.sch {
            Some(s) => s,
            None => {
                let msg = if self.nl.is_some() { tr("Nothing to draw") } else { tr("Drop a Liberty file and a gate-level netlist here") };
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, msg, egui::FontId::proportional(18.0), ui.visuals().weak_text_color());
                return;
            }
        };
        if self.sfit {
            let z = (rect.width() / sch.size.x).min(rect.height() / sch.size.y).min(1.6) * 0.95;
            self.sview.zoom = z;
            self.sview.pan = egui::Vec2::new(sch.size.x / 2.0 - rect.width() / (2.0 * z), sch.size.y / 2.0 - rect.height() / (2.0 * z));
            self.sfit = false;
            if self.exit_at.is_some() {
                eprintln!("lsv: netlist fit: view {:.0}x{:.0}, sheet {:.0}x{:.0}, zoom {:.4}", rect.width(), rect.height(), sch.size.x, sch.size.y, z);
            }
        }
        if resp.dragged_by(egui::PointerButton::Middle) {
            self.sview.pan -= resp.drag_delta() / self.sview.zoom;
        }
        if let Some(pos) = resp.hover_pos() {
            let (zf, pan) = crate::nav::wheel(ctx);
            if zf != 1.0 {
                if self.exit_at.is_some() {
                    eprintln!("lsv: netlist zoom {:.4} x {:.4} from wheel or pinch", self.sview.zoom, zf);
                }
                let before = self.sview.to_world(rect, pos);
                self.sview.zoom = (self.sview.zoom * zf).clamp(0.002, 20.0);
                let after = self.sview.to_world(rect, pos);
                self.sview.pan += before - after;
            }
            self.sview.pan -= pan / self.sview.zoom;
            let w = self.sview.to_world(rect, pos);
            self.shover = schematic::hit_test(sch, w, 5.0 / self.sview.zoom);
            // the + of a box opens it, the - of a frame closes it (ignored while a layout is running)
            let button = schematic::button_at(sch, &self.sview, rect, pos);
            if resp.clicked() {
                match button {
                    Some(g) => toggle = Some(g).filter(|_| !self.fold_busy),
                    None => {
                        self.ssel = self.shover;
                        self.live = self.shover.map(Live::Sch);
                    }
                }
            }
            if resp.double_clicked() && button.is_none() && !self.fold_busy {
                if let Some(Hit::Group(g)) = self.shover {
                    toggle = Some(g);
                }
            }
            if resp.secondary_clicked() {
                self.smenu = self.shover;
                if self.shover.is_some() {
                    self.ssel = self.shover;
                }
            }
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.ssel = None;
        }
        let vis = ui.visuals();
        let dark = vis.dark_mode;
        let col = schematic::Colors {
            ink: vis.text_color(),
            wire: if dark { egui::Color32::from_rgb(143, 165, 185) } else { egui::Color32::from_rgb(52, 73, 94) },
            muted: vis.weak_text_color(),
            hl: egui::Color32::from_rgb(212, 80, 15),
            sel: if dark { egui::Color32::from_rgb(121, 174, 245) } else { egui::Color32::from_rgb(28, 79, 140) },
            fill: vis.panel_fill,
        };
        let nl = &self.nl.as_ref().unwrap().nl;
        // many cells in view: rectangles on the GPU; few: the detailed drawing of what is visible
        // a folded sheet far out is also drawn as the overview (the detailed drawing is only specks there)
        if (self.sview.zoom < 0.08 && !sch.group_node.is_empty()) || schematic::cells_in_view(sch, &self.sview, rect) > DETAIL_CELLS {
            let zr = self.sview.zoom / self.sch_hl_zoom;
            if self.sch_hl_for != Some(self.ssel) || !(0.85..1.18).contains(&zr) {
                self.sch_hl = Arc::new(schematic::gpu_hl(sch, self.ssel, self.sview.zoom));
                self.sch_hl_version += 1;
                self.sch_hl_for = Some(self.ssel);
                self.sch_hl_zoom = self.sview.zoom;
            }
            let dpr = ctx.pixels_per_point();
            let fr = Frame { cam: schematic::gpu_cam(&self.sview, rect, dpr), styles: schematic::gpu_styles(&col), visible: vec![true; schematic::GPU_SLOTS], extra: vec![], veil_from: 0, hl: self.sch_hl.clone(), hl_version: self.sch_hl_version };
            ui.painter().add(egui_wgpu::Callback::new_paint_callback(rect, Cb { frame: fr, sch: true }));
            ui.painter().text(rect.left_bottom() + egui::vec2(8.0, -8.0), egui::Align2::LEFT_BOTTOM, trf!("overview of {} cells: zoom in for gate symbols and names", nl.cells.len().min(sch.cell_node.len())), egui::FontId::proportional(12.0), vis.weak_text_color());
            // submodule titles and +/- buttons over the overview
            let mut painter = ui.painter_at(rect);
            schematic::paint_marks(&mut painter, rect, &self.sview, sch, self.ssel, &col, true);
        } else {
            let mut painter = ui.painter_at(rect);
            schematic::paint(&mut painter, rect, &self.sview, sch, nl, self.ssel, &col);
        }
        let menu_target = self.smenu;
        let has_layout = self.data.is_some() && !matches!(menu_target, Some(Hit::Group(_)));
        let menu_open = match menu_target {
            Some(Hit::Group(g)) => Some((g, self.open.contains(&g))),
            _ => None,
        };
        let mut menu_toggle = None;
        let mut go: Option<Hit> = None;
        let mut go_cell: Option<String> = None;
        let name = menu_target.map(|h| match h {
            Hit::Cell(c) => nl.cells[c as usize].name.clone(),
            Hit::Net(n) => nl.nets[n as usize].name.clone(),
            Hit::Group(g) => sch.labels.get(&g).map(|t| t.path.clone()).unwrap_or_default(),
            Hit::Bundle(r) => schematic::bundle_nets(sch, r).0,
        });
        let master = match menu_target {
            Some(Hit::Cell(c)) => Some(nl.cells[c as usize].typ.clone()).filter(|m| self.cell.has_cell(m)),
            _ => None,
        };
        resp.context_menu(|ui| {
            if let (Some(h), Some(nm)) = (menu_target, &name) {
                ui.label(egui::RichText::new(nm).monospace().small());
                if let Some((g, open)) = menu_open {
                    if ui.button(if open { tr("Collapse") } else { tr("Expand") }).clicked() {
                        menu_toggle = Some(g);
                        ui.close();
                    }
                }
                if ui.add_enabled(has_layout, egui::Button::new(tr("Show in layout"))).clicked() {
                    go = Some(h);
                    ui.close();
                }
                if let Some(m) = &master {
                    if ui.button(trf!("Show {} transistors", m)).clicked() {
                        go_cell = Some(m.clone());
                        ui.close();
                    }
                }
            } else {
                ui.label(tr("Nothing here"));
            }
        });
        if let Some(h) = self.shover {
            let txt = match h {
                Hit::Cell(c) => format!("{}  ·  {}", nl.cells[c as usize].name, nl.cells[c as usize].typ),
                Hit::Net(n) => nl.nets[n as usize].name.clone(),
                Hit::Group(g) => sch.labels.get(&g).map(|t| if t.path.ends_with(t.title.as_str()) { trf!("{}  ·  {} cells", t.path, t.cells) } else { trf!("{}  ·  {}  ·  {} cells", t.path, t.title, t.cells) }).unwrap_or_default(),
                Hit::Bundle(r) => {
                    let (l, n) = schematic::bundle_nets(sch, r);
                    trf!("{}  ·  {} nets", l, n.len())
                }
            };
            resp.on_hover_text_at_pointer(txt);
        }
        let _ = NodeKind::Dummy;
        if let Some(h) = go {
            self.schematic_to_layout(h);
        }
        if let Some(m) = go_cell {
            self.cell.request_cell(&m);
            self.focus(Pane::Schematic);
        }
        if let Some(g) = toggle.or(menu_toggle) {
            self.toggle_group(g);
        }
    }
}
