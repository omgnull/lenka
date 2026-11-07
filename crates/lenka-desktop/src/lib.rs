// SPDX-License-Identifier: GPL-3.0-only
// Copyright (C) 2026 Igor Pie and lenka contributors

//! The native desktop app of lenka: board, tree, roadmap, Gantt chart, and drawing board.
//!
//! The app uses `eframe` with the `glow` renderer. It has no web view. It repaints only on input or
//! after a change that a background thread finds, so it uses no CPU when it is idle.

mod actions;
mod app;
mod board;
mod chart;
mod data;
mod detail;
mod dialogs;
mod drawing;
mod folder_pick;
mod fonts;
mod gantt;
mod markdown_view;
mod roadmap;
mod session;
mod settings_view;
mod theme;
mod timeline;
mod tree;
mod widgets;

#[cfg(test)]
mod shots;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod ui_tests;

/// Opens the desktop app in a native window, and returns when the window closes.
///
/// `project` selects the first project. Without it, the app uses the default project of the caller,
/// and then the first project in the user list.
///
/// # Errors
///
/// Returns an error if this machine has no usable system font, or if the native window cannot open.
pub fn run(
    host: lenka_core::Host,
    caller: lenka_plugin_api::Caller,
    project: Option<String>,
) -> anyhow::Result<()> {
    timeline::remember_local_offset();
    // Load the fonts first, so that the app opens no window with no text.
    let fonts = fonts::load_system()?;
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("lenka")
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([960.0, 600.0]),
        ..eframe::NativeOptions::default()
    };
    eframe::run_native(
        "lenka",
        options,
        Box::new(move |creation| {
            creation.egui_ctx.set_fonts(fonts);
            Ok(Box::new(app::DesktopApp::new(
                &creation.egui_ctx,
                host,
                caller,
                project,
            )))
        }),
    )
    .map_err(|err| anyhow::anyhow!("The desktop window cannot open: {err}"))
}
