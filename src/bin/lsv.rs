// no console window next to the viewer on Windows
#![cfg_attr(windows, windows_subsystem = "windows")]
//! Desktop visualizer.
//! usage: lsv [--tab netlist|layout|cell|cell-layout|cell-lef|cell-both] [--cdl cells.cdl --gds cells.gds [--cell-lef cells.lef] [--lpe cell.pex.sp] [--layermap map] [--cell NAME]] [--lib cells.lib ... --netlist design.v [--top NAME]] [--lef tech.lef --lef cells.lef ... --def design.def[.gz]] [files...]
use lsv::app::{App, Files};
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let mut files = Files::default();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let v = args.get(i + 1).cloned().unwrap_or_default();
        match args[i].as_str() {
            "--lef" => { files.lefs.push(PathBuf::from(v)); i += 1; }
            "--def" => { files.def = Some(PathBuf::from(v)); i += 1; }
            "--lib" => { files.libs.push(PathBuf::from(v)); i += 1; }
            "--netlist" => { files.netlist = Some(PathBuf::from(v)); i += 1; }
            "--top" => { files.top = Some(v); i += 1; }
            "--cdl" => { files.cdl = Some(PathBuf::from(v)); i += 1; }
            "--gds" => { files.gds = Some(PathBuf::from(v)); i += 1; }
            "--layermap" => { files.layermap = Some(PathBuf::from(v)); i += 1; }
            "--cell" => { files.cell = Some(v); i += 1; }
            "--cell-lef" => { files.cell_lef = Some(PathBuf::from(v)); i += 1; }
            "--lpe" => { files.lpe = Some(PathBuf::from(v)); i += 1; }
            "--spef" => { files.spef = Some(PathBuf::from(v)); i += 1; }
            "--tab" => { files.tab = Some(v); i += 1; }
            a => {
                let l = a.to_lowercase();
                let l = l.strip_suffix(".gz").unwrap_or(&l).to_string();
                let p = PathBuf::from(a);
                if l.ends_with(".def") { files.def = Some(p); }
                else if l.ends_with(".spef") { files.spef = Some(p); }
                else if l.ends_with(".cdl") || l.ends_with(".sp") || l.ends_with(".spice") || l.ends_with(".cir") || l.contains(".pex") || l.ends_with(".dspf") || l.ends_with(".spf") {
                    // a post-layout (LPE) netlist is read next to the CDL
                    let lpe = lsv::io::read_all(&p).map(|b| lsv::cellview::looks_like_lpe(&String::from_utf8_lossy(&b))).unwrap_or(false);
                    if lpe { files.lpe = Some(p); } else { files.cdl = Some(p); }
                }
                else if l.ends_with(".gds") || l.ends_with(".gds2") || l.ends_with(".gdsii") { files.gds = Some(p); }
                else if l.ends_with(".layermap") || l.ends_with(".lyp") { files.layermap = Some(p); }
                else if l.ends_with(".lib") { files.libs.push(p); }
                else if l.ends_with(".v") || l.ends_with(".vg") { files.netlist = Some(p); }
                else { files.lefs.push(p); }
            }
        }
        i += 1;
    }
    // UI language: the stored choice, English on the first run
    lsv::i18n::start();
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        viewport: eframe::egui::ViewportBuilder::default().with_inner_size([1500.0, 920.0]).with_drag_and_drop(true).with_title("Layout Schematic Visualizer"),
        ..Default::default()
    };
    // Wayland: mutter can leave a FIFO (vsync) frame unapplied and stop all redraws, winit still paces frames by frame callbacks
    let wayland = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|v| std::env::var(v).is_ok_and(|s| !s.is_empty()));
    if cfg!(target_os = "linux") && wayland {
        options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    }
    eframe::run_native(
        "lsv",
        options,
        Box::new(|cc| {
            // Korean, Japanese and Chinese glyphs from the fonts of the system, read when first needed
            lsv::i18n::install_fonts(&cc.egui_ctx);
            Ok(Box::new(App::new(cc, files)))
        }),
    )
}
