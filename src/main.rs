//! Audiobook Studio — a native egui/eframe app.
//! Pipeline: PDF -> (local LLM) chapter boundaries -> split + transcripts ->
//! (optional LLM polish) -> Kokoro TTS (Python sidecar) -> chaptered .m4b.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use audiobook_studio::{app, g2p, sidecar};

fn main() -> eframe::Result<()> {
    // Warm the G2P sidecar early (background) so it's ready by the time TTS runs.
    sidecar::spawn_sidecar();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([900.0, 680.0])
            .with_min_inner_size([640.0, 480.0])
            .with_title("Audiobook Studio"),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Audiobook Studio",
        options,
        Box::new(|cc| {
            // Register the Phosphor icon font as a fallback so icon glyphs in
            // labels/buttons render alongside normal text.
            let mut fonts = eframe::egui::FontDefinitions::default();
            egui_phosphor::add_to_fonts(&mut fonts, egui_phosphor::Variant::Regular);
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::<app::App>::default())
        }),
    );

    // Clean teardown of the persistent G2P child process on exit.
    g2p::shutdown();
    result
}
