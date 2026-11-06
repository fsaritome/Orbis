//! Orbis desktop app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

mod app;
mod plan;
mod wheel;
mod worker;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Orbis")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([960.0, 640.0]),
        ..Default::default()
    };
    eframe::run_native("Orbis", options, Box::new(|cc| Ok(Box::new(app::OrbisApp::new(cc)))))
}
