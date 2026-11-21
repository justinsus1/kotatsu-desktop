mod app;
mod bridge;
mod downloads;
mod images;
mod model;
mod reader;
mod store;
mod widgets;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Kotatsu")
            .with_app_id("kotatsu-desktop")
            .with_inner_size([1280.0, 860.0])
            .with_min_inner_size([480.0, 360.0]),
        ..Default::default()
    };
    eframe::run_native("kotatsu-desktop", options, Box::new(|cc| Ok(Box::new(app::App::new(cc)))))
}
