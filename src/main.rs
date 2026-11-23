#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod core;
mod ui;

fn main() -> Result<(), eframe::Error> {
    let rgba = include_bytes!("../assets/comparator.rgba").to_vec();
    let icon = eframe::egui::IconData {
        rgba,
        width: 64,
        height: 64,
    };
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Comparator Studio")
            .with_app_id("app.comparator.studio")
            .with_inner_size([900., 670.])
            .with_min_inner_size([720., 510.])
            .with_icon(icon),
        ..Default::default()
    };
    eframe::run_native(
        "Comparator Studio",
        options,
        Box::new(|cc| Ok(Box::new(ui::ComparatorApp::new(&cc.egui_ctx)))),
    )
}
