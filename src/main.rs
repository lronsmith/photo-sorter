#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use eframe::egui;
use photo_sorter::app::{PhotoSorterApp, install_fallback_fonts};

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Photo Sorter")
            .with_inner_size([1200.0, 800.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Photo Sorter",
        options,
        Box::new(|cc| {
            install_fallback_fonts(&cc.egui_ctx);
            Ok(Box::new(PhotoSorterApp::default()))
        }),
    )
}
