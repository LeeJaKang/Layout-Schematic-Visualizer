//! Headless check of the window chrome with synthetic input (no display needed): the real menu bar,
//! tab groups and dialogs are drawn into an egui context, File and View are clicked, and the painted
//! texts are listed. Also checks that a wheel-button drag pans and a left click only clicks, and that the + of
//! a submodule box in the netlist view opens it into a frame and the frame's - closes it again.
use eframe::egui::{self, Event, Modifiers, MouseWheelSource, MouseWheelUnit, PointerButton, Pos2, Rect, TouchPhase, Vec2};
use lsv::app::{dialogs, menu_bar, tool_tabs, Cmd, MenuState, Pane, Tab};
use lsv::i18n::Lang;

fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(String, Rect)> {
    fn walk(s: &egui::Shape, out: &mut Vec<(String, Rect)>) {
        match s {
            egui::Shape::Text(t) => out.push((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
            egui::Shape::Vec(v) => v.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    let mut out = vec![];
    for c in shapes {
        walk(&c.shape, &mut out);
    }
    out
}

/// one frame of the netlist view (fitted to the window): the painted texts, the click on a +/- button, the
/// view and canvas used
fn fold_frame(ctx: &egui::Context, t: f64, events: Vec<Event>, lay: &lsv::schematic::SchLayout, nl: &lsv::netlist::Netlist) -> (Vec<(String, Rect)>, Option<u32>, lsv::schematic::View, Rect) {
    use lsv::schematic::{button_at, paint, Colors, View};
    let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))), time: Some(t), events, ..Default::default() };
    let col = Colors { ink: egui::Color32::BLACK, wire: egui::Color32::DARK_BLUE, muted: egui::Color32::GRAY, hl: egui::Color32::RED, sel: egui::Color32::BLUE, fill: egui::Color32::WHITE };
    let mut toggled = None;
    let mut used = (View { pan: Vec2::ZERO, zoom: 1.0 }, Rect::NOTHING);
    let out = ctx.run_ui(input, |root| {
        egui::CentralPanel::default().show(root, |ui| {
            let rect = ui.max_rect();
            let resp = ui.allocate_rect(rect, egui::Sense::click());
            let z = (rect.width() / lay.size.x).min(rect.height() / lay.size.y).min(1.6) * 0.95;
            let view = View { pan: Vec2::new(lay.size.x / 2.0 - rect.width() / (2.0 * z), lay.size.y / 2.0 - rect.height() / (2.0 * z)), zoom: z };
            if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    toggled = button_at(lay, &view, rect, p);
                }
            }
            let mut painter = ui.painter_at(rect);
            paint(&mut painter, rect, &view, lay, nl, None, &col);
            used = (View { pan: view.pan, zoom: view.zoom }, rect);
        });
    });
    (texts(&out.shapes), toggled, used.0, used.1)
}

struct FoldSim {
    ctx: egui::Context,
    t: f64,
}

impl FoldSim {
    fn step(&mut self, lay: &lsv::schematic::SchLayout, nl: &lsv::netlist::Netlist, events: Vec<Event>) -> (Vec<(String, Rect)>, Option<u32>, lsv::schematic::View, Rect) {
        self.t += 0.1;
        fold_frame(&self.ctx, self.t, events, lay, nl)
    }
    /// a click on the +/- button of group g: the group the canvas reports as toggled
    fn click_button(&mut self, lay: &lsv::schematic::SchLayout, nl: &lsv::netlist::Netlist, g: u32) -> (Option<u32>, String) {
        let (_, _, v, r) = self.step(lay, nl, vec![]);
        let node = &lay.nodes[lay.group_node[&g]];
        let b = match lsv::schematic::fold_button(node, &v, r) {
            Some(b) => b,
            None => return (None, "no button drawn".into()),
        };
        let p = b.center();
        self.step(lay, nl, vec![Event::PointerMoved(p)]);
        self.step(lay, nl, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
        let (_, hit, _, _) = self.step(lay, nl, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
        (hit, format!("button at {:.0},{:.0}", p.x, p.y))
    }
}

struct Sim {
    ctx: egui::Context,
    t: f64,
    keys: bool,
    about: bool,
    cmds: Vec<Cmd>,
    last: Vec<(String, Rect)>,
    mid_drag: f32,
    left_drag: f32,
    clicks: usize,
    zoom: f32,
    pan: Vec2,
}

impl Sim {
    fn frame(&mut self, events: Vec<Event>) {
        self.t += 0.1;
        let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))), time: Some(self.t), events, ..Default::default() };
        let st = MenuState { tab: Tab::Cell, visible: vec![Pane::Schematic, Pane::Details], popped: vec![], side_by_side: false, show_side: true, has_lpe: false, theme: egui::ThemePreference::System, has_sel: false };
        let (keys, about, cmds) = (&mut self.keys, &mut self.about, &mut self.cmds);
        let (mut md, mut ld, mut ck) = (0.0, 0.0, 0);
        let (mut zf, mut pn) = (1.0f32, Vec2::ZERO);
        let out = self.ctx.run_ui(input, |root| {
            let c = root.ctx().clone();
            egui::Panel::top("menu").show(root, |ui| menu_bar(ui, &st, cmds));
            egui::Panel::top("bar").show(root, |ui| {
                ui.horizontal(|ui| tool_tabs(ui, &st, cmds));
            });
            dialogs(&c, keys, about);
            egui::CentralPanel::default().show(root, |ui| {
                let r = ui.max_rect();
                let resp = ui.allocate_rect(r, egui::Sense::click_and_drag());
                if resp.dragged_by(PointerButton::Middle) {
                    md += resp.drag_delta().length();
                }
                if resp.dragged_by(PointerButton::Primary) {
                    ld += resp.drag_delta().length();
                }
                if resp.clicked() {
                    ck += 1;
                }
                let (z, p) = lsv::nav::wheel(ui.ctx());
                zf = z;
                pn = p;
            });
        });
        self.zoom *= zf;
        self.pan += pn;
        self.mid_drag += md;
        self.left_drag += ld;
        self.clicks += ck;
        self.last = texts(&out.shapes);
    }
    fn names(&self) -> Vec<String> {
        self.last.iter().map(|t| t.0.clone()).collect()
    }
    fn pos_of(&self, name: &str) -> Pos2 {
        self.last.iter().find(|t| t.0 == name).unwrap_or_else(|| panic!("text {:?} is not on screen: {:?}", name, self.names())).1.center()
    }
    fn click(&mut self, p: Pos2, b: PointerButton) {
        self.frame(vec![Event::PointerMoved(p)]);
        self.frame(vec![Event::PointerButton { pos: p, button: b, pressed: true, modifiers: Modifiers::NONE }]);
        self.frame(vec![Event::PointerButton { pos: p, button: b, pressed: false, modifiers: Modifiers::NONE }]);
        self.frame(vec![]);
        self.frame(vec![]);
    }
    fn drag(&mut self, a: Pos2, b: Pos2, btn: PointerButton) {
        self.frame(vec![Event::PointerMoved(a)]);
        self.frame(vec![Event::PointerButton { pos: a, button: btn, pressed: true, modifiers: Modifiers::NONE }]);
        for k in 1..=5 {
            self.frame(vec![Event::PointerMoved(a + (b - a) * (k as f32 / 5.0))]);
        }
        self.frame(vec![Event::PointerButton { pos: b, button: btn, pressed: false, modifiers: Modifiers::NONE }]);
        self.frame(vec![]);
    }
}

fn main() {
    let mut s = Sim { ctx: egui::Context::default(), t: 0.0, keys: true, about: false, cmds: vec![], last: vec![], mid_drag: 0.0, left_drag: 0.0, clicks: 0, zoom: 1.0, pan: Vec2::ZERO };
    let mut fails = 0;
    let mut expect = |what: &str, ok: bool, detail: String| {
        println!("{}  {}{}", if ok { "PASS" } else { "FAIL" }, what, if detail.is_empty() { String::new() } else { format!("  [{}]", detail) });
        if !ok {
            fails += 1;
        }
    };
    for _ in 0..3 {
        s.frame(vec![]);
    }
    let n = s.names();
    let has = |n: &Vec<String>, list: &[&str]| -> Vec<String> { list.iter().filter(|x| !n.iter().any(|y| y == *x)).map(|x| x.to_string()).collect() };
    let miss = has(&n, &["File", "Edit", "View", "Help", "BLOCK", "Netlist", "Layout", "CELL", "Schematic", "LEF"]);
    expect("menu bar and tab groups are drawn", miss.is_empty(), format!("missing {:?}", miss));
    let miss = has(&n, &["Keyboard shortcuts", "Open files", "Wheel-button drag", "Block netlist / layout", "Cell schematic / layout / LEF", "Drag a view tab"]);
    expect("shortcuts window is drawn when its flag is set", miss.is_empty(), format!("missing {:?}", miss));
    expect("both groups have a Layout tab", n.iter().filter(|x| *x == "Layout").count() == 2, format!("{:?}", n));
    s.keys = false;
    s.frame(vec![]);

    let p = s.pos_of("File");
    s.click(p, PointerButton::Primary);
    let n = s.names();
    let miss = has(&n, &["Open…", "Block", "Open Liberty (.lib)…", "Open gate-level netlist (.v)…", "Open LEF…", "Open DEF…", "Cell", "Open CDL / SPICE / LPE netlist…", "Open GDSII…", "Open cell LEF…", "Open layer map…", "Reload all files", "Exit"]);
    expect("File menu opens with its entries", miss.is_empty(), format!("missing {:?}", miss));
    println!("      File menu texts: {:?}", n.iter().filter(|x| !["File", "Edit", "View", "Help", "BLOCK", "CELL", "Netlist", "Layout", "Schematic", "LEF"].contains(&x.as_str())).collect::<Vec<_>>());

    let p = s.pos_of("View");
    s.click(p, PointerButton::Primary);
    if !s.names().iter().any(|x| x == "Fit to window") {
        // the first click may only have closed the File menu
        let p = s.pos_of("View");
        s.click(p, PointerButton::Primary);
    }
    let n = s.names();
    let miss = has(&n, &["Block", "Cell", "Schematic", "Parasitics (LPE)", "Schematic and layout side by side", "Own window", "Dock all windows", "Reset layout", "Fit to window", "Details panel", "Theme"]);
    expect("View menu opens with its entries", miss.is_empty(), format!("missing {:?}", miss));
    // the open View menu has Layout under Block and under Cell; the lowest Layout on screen is the cell one
    let lay: Vec<Rect> = s.last.iter().filter(|t| t.0 == "Layout").map(|t| t.1).collect();
    expect("View menu and toolbar show four Layout entries", lay.len() == 4, format!("{}", lay.len()));
    let menu_layout = lay.iter().max_by(|a, b| a.center().y.partial_cmp(&b.center().y).unwrap()).unwrap().center();
    s.cmds.clear();
    s.click(menu_layout, PointerButton::Primary);
    expect("clicking View > Cell > Layout gives the cell layout command", s.cmds.contains(&Cmd::Focus(Pane::CellLayout)), format!("{:?}", s.cmds));
    expect("the menu closes after the click", !s.names().iter().any(|x| x == "Fit to window"), String::new());
    // toolbar: BLOCK Netlist, Layout and CELL Schematic, Layout, left to right
    let mut bar: Vec<(String, Rect)> = s.last.iter().filter(|t| ["Netlist", "Layout", "Schematic", "LEF"].contains(&t.0.as_str())).cloned().collect();
    bar.sort_by(|a, b| a.1.center().x.partial_cmp(&b.1.center().x).unwrap());
    let order: Vec<&str> = bar.iter().map(|t| t.0.as_str()).collect();
    expect("toolbar tabs read Netlist, Layout | Schematic, Layout, LEF", order == ["Netlist", "Layout", "Schematic", "Layout", "LEF"], format!("{:?}", order));
    if order.len() == 5 {
        let want = [Cmd::Focus(Pane::Netlist), Cmd::Focus(Pane::BlockLayout), Cmd::Focus(Pane::Schematic), Cmd::Focus(Pane::CellLayout), Cmd::Focus(Pane::CellLef)];
        let mut got = vec![];
        for (t, _) in bar.iter().zip(want.iter()) {
            s.cmds.clear();
            s.click(t.1.center(), PointerButton::Primary);
            got.push(s.cmds.clone());
        }
        let ok = got.iter().zip(want.iter()).all(|(g, w)| g.contains(w));
        expect("each toolbar tab gives its own view command", ok, format!("{:?}", got));
    }

    let p = s.pos_of("Help");
    s.click(p, PointerButton::Primary);
    let p = s.pos_of("About");
    s.cmds.clear();
    s.click(p, PointerButton::Primary);
    expect("Help > About gives the About command", s.cmds.contains(&Cmd::About), format!("{:?}", s.cmds));
    s.about = true;
    s.frame(vec![]);
    s.frame(vec![]);
    expect("About window is drawn", s.names().iter().any(|x| x.starts_with("Layout Schematic Visualizer")), String::new());
    s.about = false;
    s.frame(vec![]);

    // View > Language: the seven languages in their own scripts, a pick gives its command
    let p = s.pos_of("View");
    s.click(p, PointerButton::Primary);
    if !s.names().iter().any(|x| x == "Language") {
        let p = s.pos_of("View");
        s.click(p, PointerButton::Primary);
    }
    let p = s.pos_of("Language");
    s.click(p, PointerButton::Primary);
    let names: Vec<&str> = lsv::i18n::LANGS.iter().map(|l| l.2).collect();
    let miss = has(&s.names(), &names);
    expect("View > Language lists English, Korean, Japanese, Chinese, Spanish, German and French", miss.is_empty() && names.len() == 7, format!("missing {:?}", miss));
    s.cmds.clear();
    let p = s.pos_of("Deutsch");
    s.click(p, PointerButton::Primary);
    expect("picking Deutsch gives the German language command", s.cmds.contains(&Cmd::Lang(Lang::De)), format!("{:?}", s.cmds));
    // labels follow the language, untranslated ones stay English
    lsv::i18n::set_lang(Lang::Ko);
    s.keys = true;
    s.frame(vec![]);
    s.frame(vec![]);
    let n = s.names();
    let miss = has(&n, &["파일", "편집", "보기", "도움말", "블록", "넷리스트", "레이아웃", "회로도", "LEF", "단축키", "파일 열기"]);
    expect("in Korean the menu bar, tabs and shortcuts window are Korean", miss.is_empty(), format!("missing {:?}", miss));
    lsv::i18n::set_lang(Lang::En);
    s.keys = false;
    s.frame(vec![]);
    expect("back in English the menu bar reads File", s.names().iter().any(|x| x == "File"), String::new());

    // pointer behaviour on a canvas
    let (a, b) = (Pos2::new(500.0, 500.0), Pos2::new(620.0, 560.0));
    s.mid_drag = 0.0;
    s.left_drag = 0.0;
    s.clicks = 0;
    s.drag(a, b, PointerButton::Middle);
    expect("wheel-button drag is reported as a drag (pans)", s.mid_drag > 100.0, format!("moved {:.0} px", s.mid_drag));
    expect("wheel-button drag does not click", s.clicks == 0, format!("clicks {}", s.clicks));
    s.mid_drag = 0.0;
    s.drag(a, b, PointerButton::Primary);
    expect("left drag is not seen as a wheel drag (does not pan)", s.mid_drag == 0.0, format!("moved {:.0} px", s.mid_drag));
    s.clicks = 0;
    s.click(a, PointerButton::Primary);
    expect("left click clicks once", s.clicks == 1, format!("clicks {}", s.clicks));
    s.clicks = 0;
    s.mid_drag = 0.0;
    // a click with a small slip of the hand (3 px)
    s.frame(vec![Event::PointerMoved(a)]);
    s.frame(vec![Event::PointerButton { pos: a, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE }]);
    s.frame(vec![Event::PointerMoved(a + egui::vec2(3.0, 1.0))]);
    s.frame(vec![Event::PointerButton { pos: a + egui::vec2(3.0, 1.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE }]);
    s.frame(vec![]);
    expect("left click with a 3 px slip still clicks and does not pan", s.clicks == 1 && s.mid_drag == 0.0, format!("clicks {} pan {:.0}", s.clicks, s.mid_drag));
    // wheel and touchpad: a mouse wheel zooms, two fingers on a touchpad pan (MacBook without a mouse)
    let wheel = |unit: MouseWheelUnit, source: MouseWheelSource, dy: f32, mods: Modifiers| Event::MouseWheel { unit, delta: egui::vec2(0.0, dy), phase: TouchPhase::Move, source, modifiers: mods };
    let try_wheel = |s: &mut Sim, e: Event| -> (f32, Vec2) {
        s.frame(vec![Event::PointerMoved(a)]);
        s.zoom = 1.0;
        s.pan = Vec2::ZERO;
        s.frame(vec![e]);
        (s.zoom, s.pan)
    };
    let (z, p) = try_wheel(&mut s, wheel(MouseWheelUnit::Line, MouseWheelSource::Unknown, 1.0, Modifiers::NONE));
    expect("mouse wheel one step (Windows, Linux) zooms and does not pan", z > 1.05 && p == Vec2::ZERO, format!("zoom {:.3} pan {:?}", z, p));
    let (z, p) = try_wheel(&mut s, wheel(MouseWheelUnit::Point, MouseWheelSource::Unknown, -40.0, Modifiers::NONE));
    expect("touchpad scroll in points (Windows, Linux) pans and does not zoom", z == 1.0 && p.y == -40.0, format!("zoom {:.3} pan {:?}", z, p));
    let (z, p) = try_wheel(&mut s, wheel(MouseWheelUnit::Point, MouseWheelSource::Trackpad, 30.0, Modifiers::NONE));
    expect("MacBook trackpad two-finger scroll pans", z == 1.0 && p.y == 30.0, format!("zoom {:.3} pan {:?}", z, p));
    let (z, p) = try_wheel(&mut s, wheel(MouseWheelUnit::Point, MouseWheelSource::Wheel, 40.0, Modifiers::NONE));
    expect("mouse wheel on macOS (points, source wheel) zooms", z > 1.05 && p == Vec2::ZERO, format!("zoom {:.3} pan {:?}", z, p));
    let (z, p) = try_wheel(&mut s, Event::Zoom(1.25));
    expect("pinch zooms", (z - 1.25).abs() < 1e-3 && p == Vec2::ZERO, format!("zoom {:.3} pan {:?}", z, p));

    // netlist view folding on tests/hier_adder.v (top -> u_add -> fa0, fa1 -> h0, h1, and top -> u_par)
    {
        use lsv::fold::{view, Cache, Fold};
        let (_, mods) = lsv::verilog::parse_verilog(include_str!("../../tests/hier_adder.v").as_bytes(), "hier_adder.v").unwrap();
        let nl = lsv::netlist::elaborate(&mods, "hier_top", &std::collections::HashMap::new()).unwrap();
        let fold = Fold::build(&nl);
        let gid = |p: &str| fold.groups.iter().position(|g| g.path == p).map(|g| g as u32).unwrap_or_else(|| panic!("no group {}", p));
        let (u_add, fa0) = (gid("u_add"), gid("u_add/fa0"));
        let mut open = std::collections::HashSet::new();
        let mut cache = Cache::new();
        let mut fs = FoldSim { ctx: egui::Context::default(), t: 0.0 };
        let mut lay = view(&nl, &fold, &open, &mut cache);
        fs.step(&lay, &nl, vec![]);
        let (n, _, _, _) = fs.step(&lay, &nl, vec![]);
        let names: Vec<String> = n.iter().map(|x| x.0.clone()).collect();
        let boxes_closed = names.iter().any(|x| x == "add2") && names.iter().any(|x| x == "par") && !names.iter().any(|x| x.contains("u_add/fa0"));
        expect("netlist view starts with submodules closed (boxes add2 and par)", boxes_closed && lay.cell_node.len() == 8, format!("{} cells drawn, texts {:?}", lay.cell_node.len(), names));
        // the 4-bit bus mid between the closed boxes u_buf and u_neg is one bundle wire on one pin row each side
        {
            let mid: std::collections::HashSet<u32> = (0..4).map(|i| nl.net_by_name[&format!("mid[{}]", i)]).collect();
            let bundles: Vec<&lsv::schematic::Bundle> = lay.bundles.iter().filter(|b| b.at.is_some() && b.nets.iter().any(|n| mid.contains(n))).collect();
            let single = lay.wires.iter().filter(|w| !w.bundle && mid.contains(&w.net)).count();
            let ok = bundles.len() == 1 && bundles[0].nets.len() == 4 && bundles[0].label == "mid[3:0]" && single == 0;
            expect("the bus mid[3:0] between two boxes is one bundle wire", ok, format!("{} bundles {:?}, {} single wires", bundles.len(), bundles.iter().map(|b| (&b.label, b.nets.len())).collect::<Vec<_>>(), single));
            let u_buf = gid("u_buf");
            let bx = &lay.nodes[lay.group_node[&u_buf]];
            let rows: std::collections::HashSet<u32> = bx.outs.iter().filter(|x| mid.contains(&x.0)).map(|x| x.1.to_bits()).collect();
            expect("u_buf has one pin row o[3:0] for the bus", rows.len() == 1 && bx.pin_names_out.iter().all(|x| x == "o[3:0]"), format!("{} rows, {:?}", rows.len(), bx.pin_names_out));
            if let Some(b) = bundles.first() {
                let hit = lsv::schematic::hit_test(&lay, b.at.unwrap(), 4.0);
                expect("a click on the bundle wire picks the bundle", hit == Some(lsv::schematic::Hit::Bundle(b.rep)), format!("{:?}", hit));
            }
        }
        let (hit, d) = fs.click_button(&lay, &nl, u_add);
        expect("clicking + of the u_add box asks to open u_add", hit == Some(u_add), format!("{:?} {}", hit, d));
        if let Some(g) = hit {
            fold.toggle(&mut open, g);
        }
        lay = view(&nl, &fold, &open, &mut cache);
        let (n, _, _, _) = fs.step(&lay, &nl, vec![]);
        let names: Vec<String> = n.iter().map(|x| x.0.clone()).collect();
        let opened = names.iter().any(|x| x == "add2  ·  u_add") && names.iter().filter(|x| *x == "fa").count() == 2;
        expect("u_add is drawn as a frame holding two closed fa boxes", opened, format!("{:?}", names));
        let (hit, d) = fs.click_button(&lay, &nl, fa0);
        expect("clicking + of the fa0 box inside the frame asks to open it", hit == Some(fa0), format!("{:?} {}", hit, d));
        if let Some(g) = hit {
            fold.toggle(&mut open, g);
        }
        lay = view(&nl, &fold, &open, &mut cache);
        let (n, _, _, _) = fs.step(&lay, &nl, vec![]);
        let names: Vec<String> = n.iter().map(|x| x.0.clone()).collect();
        let nested = names.iter().any(|x| x == "fa  ·  u_add/fa0") && names.iter().filter(|x| *x == "ha").count() == 2 && lay.cell_node.len() == 9;
        expect("fa0 opens inside the u_add frame with its two ha boxes and its OR gate", nested, format!("{} cells drawn, {:?}", lay.cell_node.len(), names));
        // the top sheet draws ports, so every stub belongs to (lies inside) a frame
        use lsv::schematic::NodeKind;
        let frames: Vec<Rect> = lay.nodes.iter().filter(|n| matches!(n.kind, NodeKind::Frame(_))).map(|n| Rect::from_min_size(n.pos, egui::vec2(n.w, n.h))).collect();
        let loose: Vec<String> = lay.nodes.iter().filter(|n| matches!(n.kind, NodeKind::InStub(_) | NodeKind::OutStub(_)) && !frames.iter().any(|f| f.contains(n.pos))).map(|n| format!("{:?}", n.kind)).collect();
        expect("no stub outside the frames on the top sheet", loose.is_empty(), format!("{:?}", loose));
        // the open u_add frame has the pins of add2 (5 inputs, 3 outputs), not those of fa0 opened inside it
        let fr = &lay.nodes[lay.group_node[&u_add]];
        let distinct = |v: &Vec<(u32, f32)>| v.iter().map(|x| x.0).collect::<std::collections::HashSet<u32>>().len();
        let (fi, fo) = (distinct(&fr.ins), distinct(&fr.outs));
        expect("the u_add frame has 5 input and 3 output nets with fa0 open inside it", fi == 5 && fo == 3, format!("{} inputs, {} outputs", fi, fo));
        let (hit, d) = fs.click_button(&lay, &nl, u_add);
        expect("clicking - of the u_add frame asks to close it", hit == Some(u_add), format!("{:?} {}", hit, d));
        if let Some(g) = hit {
            fold.toggle(&mut open, g);
        }
        lay = view(&nl, &fold, &open, &mut cache);
        let (n, _, _, _) = fs.step(&lay, &nl, vec![]);
        let names: Vec<String> = n.iter().map(|x| x.0.clone()).collect();
        expect("after - the u_add box is closed again with nothing opened inside", open.is_empty() && names.iter().any(|x| x == "add2") && !names.iter().any(|x| x.contains("·  u_add")), format!("open {:?}, {:?}", open, names));
    }
    // logic check in the cell Details: the Liberty function passes on every vector, a wrong one is caught
    {
        const NAND2: &str = "
.SUBCKT NAND2 A B Y VDD VSS
MP1 Y A VDD VDD pmos w=1u l=0.1u
MP2 Y B VDD VDD pmos w=1u l=0.1u
MN1 Y A n1 VSS nmos w=1u l=0.1u
MN2 n1 B VSS VSS nmos w=1u l=0.1u
.ENDS
";
        let panel = |ct: &mut lsv::cellview::CellTab| -> Vec<String> {
            let ctx = egui::Context::default();
            let mut out = vec![];
            for t in [0.0, 0.1] {
                let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(520.0, 1600.0))), time: Some(t), ..Default::default() };
                let o = ctx.run_ui(input, |root| {
                    egui::CentralPanel::default().show(root, |ui| ct.side_panel(ui));
                });
                out = texts(&o.shapes).into_iter().map(|x| x.0).collect();
            }
            out
        };
        let mut ct = lsv::cellview::CellTab::new(Default::default());
        ct.cdl = Some(lsv::cdl::parse_cdl(NAND2, "nand2.cdl"));
        ct.names = vec!["NAND2".into()];
        ct.lib_funcs.insert("NAND2".into(), Ok("Y = !(A & B)".into()));
        ct.show_cell("NAND2");
        let sch = lsv::i18n::tr("schematic");
        let shown = panel(&mut ct);
        let ok = lsv::trf!("{}: all {} input vectors match", sch, 4);
        expect("logic check: the Liberty function of a NAND2 matches on all 4 input vectors", shown.contains(&ok) && shown.iter().any(|x| x == "Y = !(A & B)"), format!("{:?}", shown.iter().filter(|x| x.contains("vector") || x.contains('=')).collect::<Vec<_>>()));
        ct.func_text.insert("NAND2".into(), "Y = A & B".into());
        ct.run_logic();
        let shown = panel(&mut ct);
        let bad = lsv::trf!("{}: {} of {} input vectors differ", sch, 4, 4);
        let header = format!("Y · {}", sch);
        expect("logic check: Y = A & B on the NAND2 is caught on 4 of 4 vectors, with the truth table", shown.contains(&bad) && shown.contains(&header), format!("{:?}", shown.iter().filter(|x| x.contains("vector") || x.contains('·')).collect::<Vec<_>>()));
    }
    // logic check with the real background loader: a .lib that is not Liberty does not stop the cells, and a
    // Liberty read after a cell was shown (here by a full reload of the fixed file) fills that cell's functions
    {
        let dir = std::env::temp_dir().join(format!("lsv-uitest-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let (cdl, lib) = (dir.join("nand2.cdl"), dir.join("cells.lib"));
        let nand2 = ".SUBCKT NAND2 A B Y VDD VSS\nMP1 Y A VDD VDD pmos w=1u l=0.1u\nMP2 Y B VDD VDD pmos w=1u l=0.1u\nMN1 Y A n1 VSS nmos w=1u l=0.1u\nMN2 n1 B VSS VSS nmos w=1u l=0.1u\n.ENDS\n";
        let _ = std::fs::write(&cdl, nand2);
        let _ = std::fs::write(&lib, "* SPICE model library\n.model nch nmos level=1\n");
        let ctx = egui::Context::default();
        let files = lsv::cellview::CellFiles { cdl: Some(cdl.clone()), libs: vec![lib.clone()], ..Default::default() };
        let mut ct = lsv::cellview::CellTab::new(files);
        let wait = |ct: &mut lsv::cellview::CellTab| {
            for _ in 0..500 {
                if ct.poll().is_some() {
                    return true;
                }
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            false
        };
        ct.start_load(&ctx, true, true, false, false, false);
        let done = wait(&mut ct);
        expect("a .lib that is not Liberty is noted and the CDL still loads", done && ct.cdl.is_some() && ct.cell.as_deref() == Some("NAND2") && ct.status.contains("Liberty"), ct.status.clone());
        let _ = std::fs::write(&lib, "library (t) {\n  cell (NAND2) {\n    pin (A) { direction : input ; }\n    pin (B) { direction : input ; }\n    pin (Y) { direction : output ; function : \"!(A & B)\" ; }\n  }\n}\n");
        ct.start_load(&ctx, true, true, false, false, false);
        let done = wait(&mut ct);
        let text = ct.func_text.get("NAND2").cloned().unwrap_or_default();
        let ok = matches!(&ct.func_out, Some(Ok(o)) if o.differ(0) == Some(0));
        expect("after the Liberty is fixed and reloaded, the cell shown gets its function and matches", done && text == "Y = !(A & B)" && ok, format!("{:?} {:?}", text, ct.func_out.as_ref().map(|r| r.as_ref().map(|o| o.differ(0)))));
        ct.files.libs.push(dir.join("more.lib"));
        expect("a Liberty file added later is pending until read", ct.libs_pending(), String::new());
        for f in [&cdl, &lib] {
            let _ = std::fs::remove_file(f);
        }
        let _ = std::fs::remove_dir(&dir);
    }
    println!("{}", if fails == 0 { "ALL PASS".to_string() } else { format!("{} FAILED", fails) });
    std::process::exit(if fails == 0 { 0 } else { 1 });
}
