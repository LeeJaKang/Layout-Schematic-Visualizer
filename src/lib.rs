//! Readers and geometry for the desktop visualizer.
pub mod def;
pub mod io;
pub mod lef;
pub mod tok;
pub mod scene;
pub mod gpu;
pub mod app;
pub mod liberty;
pub mod logic;
pub mod cells;
pub mod verilog;
pub mod netlist;
pub mod schematic;
pub mod fold;
pub mod gds;
pub mod cdl;
pub mod extract;
pub mod devmatch;
pub mod xtor;
pub mod cellview;
pub mod nav;

/// Shape kinds, same numbering as the web viewer.
pub const K_WIRE: u8 = 1;
pub const K_VIA: u8 = 2;
pub const K_SWIRE: u8 = 3;
pub const K_SVIA: u8 = 4;
pub const K_IOPIN: u8 = 5;
pub const K_BLOCK: u8 = 6;
pub mod lpe;
pub mod spef;
pub mod pdk;
pub mod i18n;
pub mod func;
