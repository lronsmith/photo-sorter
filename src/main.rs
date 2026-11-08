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
    let result = eframe::run_native(
        "Photo Sorter",
        options,
        Box::new(|cc| {
            install_fallback_fonts(&cc.egui_ctx);
            Ok(Box::new(PhotoSorterApp::default()))
        }),
    );
    if let Err(e) = &result {
        // The release build has no console, so an error returned here would
        // otherwise vanish without a trace.
        show_error(&format!("Photo Sorter couldn't start: {e}"));
    }
    result
}

/// Shows `text` in a modal Windows message box.
fn show_error(text: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
    use windows::core::PCWSTR;

    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let text = wide(text);
    let caption = wide("Photo Sorter");
    // SAFETY: both buffers are NUL-terminated UTF-16 and outlive the call.
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}
