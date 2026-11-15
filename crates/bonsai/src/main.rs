//! Bonsai, a native Git client for macOS.

mod ui;
mod welcome;

use ui::{
    app::{AppConfig, run},
    app_theme,
};

fn main() {
    run(
        AppConfig::new("Bonsai")
            .themes(app_theme::light(), app_theme::dark())
            .appearance(app_theme::APPEARANCE),
        |_, _| welcome::Welcome,
    );
}
