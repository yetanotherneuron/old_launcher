// A release build on Windows is a GUI app: no console window beside it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use iced::{Size, window};
use old_launcher::App;

pub fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title(App::title)
        .theme(App::theme)
        .subscription(App::subscription)
        .window(window::Settings {
            size: Size::new(1100.0, 780.0),
            min_size: Some(App::MIN_SIZE),
            ..window::Settings::default()
        })
        .centered()
        .run()
}
