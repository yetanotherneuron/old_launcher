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
        // The launcher quits itself, after asking when something is under
        // way that quitting would cut short.
        exit_on_close_request: false,
        ..window::Settings::default()
    };

    // The Wayland app id and X11 window class, which desktops match against
    // `assets/old_launcher.desktop` to show the launcher's name and icon.
    #[cfg(target_os = "linux")]
    {
        window.platform_specific.application_id = "old_launcher".to_owned();
    }

    // Which fonts the system has is read from an index in the launcher's
    // cache, not by parsing every font file on the window's thread (see the
    // patches in `Cargo.toml`). It must come before any text is laid out.
    iced::advanced::graphics::text::set_font_cache(old_launcher::font_cache());

    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .fonts(old_launcher::fonts())
        .font(old_launcher::FONT)
        .subscription(App::subscription)
        .window(window)
        .centered()
        .run()
}
