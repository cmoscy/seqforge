mod app;
mod browser;
mod tabs;
mod viewer;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        persist_window: true,
        ..Default::default()
    };
    eframe::run_native(
        "SeqForge",
        options,
        Box::new(|cc| Ok(Box::new(app::SeqForgeApp::new(cc)))),
    )
}
