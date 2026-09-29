// A release build on Windows is a GUI app: no console window beside it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use iced::{Size, window};
use old_launcher::App;

pub fn main() -> iced::Result {
    // Quiet unless asked: `RUST_LOG=old_launcher=info` tells what the
    // launcher does, and the banner's errors are logged either way.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    #[allow(unused_mut)]
    let mut window = window::Settings {
        size: Size::new(1100.0, 780.0),
        min_size: Some(App::MIN_SIZE),
        icon: old_launcher::icon(),
        ..window::Settings::default()
    };

    // The Wayland app id and X11 window class, which desktops match against
    // `assets/old_launcher.desktop` to show the launcher's name and icon.
    #[cfg(target_os = "linux")]
    {
        window.platform_specific.application_id = "old_launcher".to_owned();
    }

    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window)
        .centered()
        .run()
}
