//! Orbis desktop app.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")] // no console window in release

mod api;
mod connection;
mod app;
mod camera;
mod model;
mod plan;
mod wheel;
mod worker;

/// The release build has no console, so write panics to a file.
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if let Some(dir) = std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("Orbis")) {
            let _ = std::fs::create_dir_all(&dir);
            let thread = std::thread::current().name().unwrap_or("?").to_owned();
            let msg = format!(
                "{}  thread '{thread}' {info}\n{}\n\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                std::backtrace::Backtrace::force_capture()
            );
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("orbis-panic.log")) {
                let _ = f.write_all(msg.as_bytes());
            }
        }
        default(info);
    }));
}

fn main() -> eframe::Result {
    log_panics();
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
