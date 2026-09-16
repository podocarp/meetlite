#[path = "../gui/mod.rs"]
mod gui;

fn main() -> eframe::Result {
    gui::app::run()
}
