//! Solar Systray: the Solar Agent solar surplus view as a minimal Windows notification-area app.

#![windows_subsystem = "windows"]

mod alerts;
mod app;
mod autostart;
mod icon;
mod settings;
mod view;

fn main() {
    if let Err(error) = app::run() {
        app::message_box(&error, true);
        std::process::exit(1);
    }
}
