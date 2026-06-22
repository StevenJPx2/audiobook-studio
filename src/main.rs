//! Audiobook Studio — a native egui/eframe app.
//! Pipeline: PDF -> (local LLM) chapter boundaries -> split + transcripts ->
//! (optional LLM polish) -> Kokoro TTS (Python sidecar) -> chaptered .m4b.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use audiobook_studio::{app, bundle_env, g2p, sidecar};

fn main() -> eframe::Result<()> {
    // In a packaged .app, point HF cache at the bundled model + force offline.
    bundle_env::init();
    // Warm the G2P sidecar early (background) so it's ready by the time TTS runs.
    sidecar::spawn_sidecar();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([900.0, 680.0])
        .with_min_inner_size([640.0, 480.0])
        .with_title("Audiobook Studio");
    if let Some(icon) = load_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions {
        viewport,
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

/// Decode the embedded window/dock icon PNG into eframe's IconData. Returns
/// None if decoding fails (the app still runs, just without a custom icon).
fn load_icon() -> Option<eframe::egui::IconData> {
    let bytes = include_bytes!("../packaging/icon/window-512.png");
    let img = image::load_from_memory(bytes).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(eframe::egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}
