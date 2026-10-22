// A release build on Windows is a GUI app: no console window beside it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use iced::{Size, window};
use old_launcher::App;

pub fn main() -> iced::Result {
    // The launcher says what it does on stderr; its dependencies only when
    // something is wrong, unless `RUST_LOG` asks for more.
    env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or("warn,old_launcher=info"),
    )
    .init();

    // Started by the launcher to run a game while Discord shows it.
    if let Some(code) = old_launcher::discord::wrapper(std::env::args_os()) {
        std::process::exit(code);
    }

    #[allow(unused_mut)]
    let mut window = window::Settings {
        size: Size::new(1200.0, 780.0),
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
