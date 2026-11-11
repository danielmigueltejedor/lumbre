//! Lumbre — the desktop assistant.
//!
//! This binary is only an interface: it holds no model and makes no decisions. Everything it
//! shows comes from the local daemon through `lumbre-api`, and when the daemon has nothing to
//! say (not running, no backend, no model) the interface says exactly that.

mod app;
mod capture;
mod worker;

use nodalia_theme::for_egui::{apply_system, Appearance};
use nodalia_theme::Accent;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title("Lumbre")
            .with_app_id("com.nodalia.Lumbre")
            .with_inner_size([1040.0, 700.0])
            .with_min_inner_size([640.0, 420.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Lumbre",
        options,
        Box::new(|creation| {
            // Nodalia Design Language, following the system's light/dark preference. Lumbre has
            // no accent of its own in NDL yet, so it uses the default Nodalia accent.
            apply_system(
                &creation.egui_ctx,
                &Appearance { accent: Accent::Nodalia, ..Appearance::default() },
            );
            Ok(Box::new(app::Lumbre::new(creation.egui_ctx.clone())))
        }),
    )
}
