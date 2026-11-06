//! Orbis desktop app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

mod app;
mod camera;
mod model;
mod plan;
mod wheel;
mod worker;

fn main() -> eframe::Result {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Orbis")
        .with_inner_size([1280.0, 800.0])
        .with_min_inner_size([960.0, 640.0]);
    // The chameleon icon in the title bar and taskbar.
    if let Ok(icon) = eframe::icon_data::from_png_bytes(include_bytes!("../../docs/img/icon-256.png")) {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("Orbis", options, Box::new(|cc| Ok(Box::new(app::OrbisApp::new(cc)))))
}
