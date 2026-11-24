//! Navigation input shared by all canvases. A mouse wheel zooms; a touchpad two-finger scroll (e.g. a
//! MacBook trackpad) pans; pinch or Ctrl/Cmd + wheel zooms. Panning with the middle button is handled by
//! each canvas.
use eframe::egui::{self, Event, MouseWheelSource, MouseWheelUnit, Vec2};

/// (zoom factor to apply at the pointer, pan in points to move the view by) for this frame
pub fn wheel(ctx: &egui::Context) -> (f32, Vec2) {
    ctx.input(|i| {
        let mut zoom = i.zoom_delta();
        let mut pan = Vec2::ZERO;
        for e in &i.events {
            if let Event::MouseWheel { unit, delta, modifiers, source, .. } = e {
                if modifiers.ctrl || modifiers.command {
                    continue; // already in zoom_delta
                }
                // macOS says what drives the scroll; elsewhere a wheel steps in lines and a touchpad in points
                let touchpad = match source {
                    MouseWheelSource::Trackpad | MouseWheelSource::Momentum => true,
                    MouseWheelSource::Wheel => false,
                    MouseWheelSource::Unknown => *unit == MouseWheelUnit::Point,
                };
                if touchpad {
                    pan += *delta;
                } else {
                    zoom *= match unit {
                        MouseWheelUnit::Line => (delta.y * 0.125).exp(),
                        MouseWheelUnit::Page => (delta.y * 0.5).exp(),
                        MouseWheelUnit::Point => (delta.y * 0.0025).exp(),
                    };
                }
            }
        }
        (zoom, pan)
    })
}
