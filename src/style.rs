//! The launcher's look: the game's own, as far as its colors, type and
//! shapes go (`docs/plans/wotlk-look.md` says what is taken and what can't
//! be), and the few components every screen shares.

use iced::theme::palette;
use iced::widget::{button, center, container, mouse_area, opaque, stack, svg, text, tooltip};
use iced::{Border, Color, Fit, Font, Theme, Widget, border, font};

use crate::backdrop::backdrop;

/// A trash can, drawn in black like every icon: the buttons tint them.
pub const TRASH: &[u8] = include_bytes!("../assets/trash.svg");

/// A plus sign, for adding.
pub const PLUS: &[u8] = include_bytes!("../assets/plus.svg");

/// A pen, for editing.
pub const PEN: &[u8] = include_bytes!("../assets/pen.svg");

/// A cog, for the settings.
pub const COG: &[u8] = include_bytes!("../assets/cog.svg");

/// The size the icons are designed at: there, their lines fall on whole
/// pixels, which any smaller size smears across two or three.
const ICON: f32 = 24.0;

/// Cinzel, under the SIL Open Font License (`assets/fonts/Cinzel-OFL.txt`):
/// capitals cut the way the game's headings are. The game's own fonts are
/// licensed, and stay its.
const CINZEL: &[u8] = include_bytes!("../assets/fonts/Cinzel-Regular.ttf");
const CINZEL_BOLD: &[u8] = include_bytes!("../assets/fonts/Cinzel-Bold.ttf");

/// The fonts the launcher brings, loaded before anything is drawn (and by
/// the screenshots): iced's own Fira Sans for the body, these for titles.
pub fn fonts() -> [&'static [u8]; 2] {
    [CINZEL, CINZEL_BOLD]
}

/// The default font in bold, for names and whatever stands out in a line.
pub const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

/// The font of titles and headings: Cinzel, bold, whose lowercase is small
/// capitals.
pub const TITLE: Font = Font {
    family: font::Family::Name("Cinzel"),
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

/// The colors of the game's interface, as its `FrameXML` names them: the
/// text colors, and the item qualities, which the badges take.
pub mod wotlk {
    use iced::{Color, color};

    /// `NORMAL_FONT_COLOR`: headings, labels, buttons.
    pub const GOLD: Color = color!(0xffd100);
    /// `HIGHLIGHT_FONT_COLOR`: the text itself.
    pub const WHITE: Color = color!(0xf5f5f5);
    /// `RED_FONT_COLOR`: errors.
    pub const RED: Color = color!(0xff1919);
    /// The near-black navy the frames are filled with, over which the text
    /// is read.
    pub const NAVY: Color = color!(0x0b0b14);

    /// Item quality: poor.
    pub const POOR: Color = color!(0x9d9d9d);
    /// Item quality: uncommon.
    pub const UNCOMMON: Color = color!(0x1eff00);
    /// Item quality: rare.
    pub const RARE: Color = color!(0x0070dd);
    /// Item quality: legendary.
    pub const LEGENDARY: Color = color!(0xff8000);
}

/// The palette a [`badge`] takes its color from: the game's item qualities,
/// for what they mean here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Gold: facts about the addon, and the launcher's own.
    Primary,
    /// Uncommon green: all good.
    Success,
    /// Legendary orange: works, but worth a look.
    Warning,
    /// Red: the client will not load it.
    Danger,
    /// Rare blue: something newer is to be had.
    Rare,
    /// Poor grey: no longer to be had.
    Poor,
}

/// The game's theme: white text on near-black navy, gold first.
pub fn theme() -> Theme {
    Theme::custom(
        "Old Launcher",
        palette::Seed {
            background: wotlk::NAVY,
            text: wotlk::WHITE,
            primary: wotlk::GOLD,
            success: wotlk::UNCOMMON,
            warning: wotlk::LEGENDARY,
            danger: wotlk::RED,
        },
    )
}

/// A title or heading, in the game's gold capitals, `size` tall.
pub fn heading<'a>(label: impl text::IntoFragment<'a>, size: f32) -> iced::widget::Text<'a> {
    text(label).size(size).font(TITLE).style(gold)
}

/// Text in the game's gold: headings and labels.
pub fn gold(_theme: &Theme) -> text::Style {
    text::Style {
        color: Some(wotlk::GOLD),
        ..text::Style::default()
    }
}

/// A raised, rounded panel for list items and dialogs.
pub fn card(theme: &Theme) -> container::Style {
    let palette = theme.palette();

    container::Style {
        background: Some(palette.background.weakest.color.into()),
        border: Border {
            color: palette.background.weak.color,
            width: 1.0,
            radius: 6.0.into(),
        },
        ..container::Style::default()
    }
}

/// `content`, explained by `hint` in a box below it on hover.
pub fn tip<'a, Message: 'a>(
    content: impl Widget<Message> + 'a,
    hint: impl text::IntoFragment<'a>,
) -> impl Widget<Message> + 'a {
    tooltip(
        content,
        container(text(hint).size(13))
            .padding([6, 10])
            .width(Fit.max(360))
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
}

/// A full-width notice tinted with `tone`, whose text takes the tone's color.
pub fn banner(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let color = tone.color(theme);

        container::Style {
            background: Some(Color { a: 0.12, ..color }.into()),
            text_color: Some(color),
            border: border::rounded(6).color(Color { a: 0.5, ..color }).width(1),
            ..container::Style::default()
        }
    }
}

impl Tone {
    /// The palette color of the tone.
    fn color(self, theme: &Theme) -> Color {
        let palette = theme.palette();

        match self {
            Self::Primary => palette.primary.base.color,
            Self::Success => palette.success.base.color,
            Self::Warning => palette.warning.base.color,
            Self::Danger => palette.danger.base.color,
            Self::Rare => wotlk::RARE,
            Self::Poor => wotlk::POOR,
        }
    }
}

/// A small outlined label ("Installed", "Load on demand", …).
pub fn badge<'a, Message: 'a>(
    label: impl text::IntoFragment<'a>,
    tone: Tone,
) -> impl Widget<Message> + 'a {
    container(text(label).size(12))
        .padding([2, 8])
        .style(move |theme: &Theme| {
            let color = tone.color(theme);

            container::Style {
                background: Some(Color { a: 0.12, ..color }.into()),
                text_color: Some(color),
                border: border::rounded(4).color(Color { a: 0.6, ..color }).width(1),
                ..container::Style::default()
            }
        })
}

/// A gold [`badge`] that links to `url`: it sends `on_press` when clicked and
/// shows the URL on hover.
pub fn link_badge<'a, Message: Clone + 'a>(
    label: &str,
    url: &'a str,
    on_press: Message,
) -> impl Widget<Message> + use<'a, Message> {
    let badge = button(text(format!("{label} ↗")).size(12))
        .padding([2, 8])
        .on_press(on_press)
        .style(|theme: &Theme, status| {
            let color = theme.palette().primary.base.color;
            let fill = match status {
                button::Status::Hovered | button::Status::Pressed => 0.3,
                button::Status::Active | button::Status::Disabled => 0.12,
            };

            button::Style {
                background: Some(Color { a: fill, ..color }.into()),
                text_color: color,
                border: border::rounded(4).color(Color { a: 0.6, ..color }).width(1),
                ..button::Style::default()
            }
        });

    tooltip(
        badge,
        container(text(url).size(12))
            .padding([4, 8])
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
}

/// A small trash can button that sends `on_press` (disabled without one),
/// and says what it deletes (`label`) on hover.
pub fn delete_button<'a, Message: Clone + 'a>(
    label: &'a str,
    on_press: Option<Message>,
) -> impl Widget<Message> + 'a {
    let enabled = on_press.is_some();

    let icon = svg(svg::Handle::from_memory(TRASH))
        .width(18)
        .height(18)
        .style(move |theme: &Theme, status| {
            let palette = theme.palette();

            svg::Style {
                color: Some(if !enabled {
                    palette.background.strong.color
                } else if status == svg::Status::Hovered {
                    palette.danger.base.color
                } else {
                    palette.background.base.text
                }),
            }
        });

    let button = button(icon)
        .padding(7)
        .on_press_maybe(on_press)
        .style(|theme: &Theme, status| {
            let danger = theme.palette().danger.base.color;

            button::Style {
                background: match status {
                    button::Status::Hovered | button::Status::Pressed => {
                        Some(Color { a: 0.15, ..danger }.into())
                    }
                    button::Status::Active | button::Status::Disabled => None,
                },
                border: border::rounded(4),
                ..button::Style::default()
            }
        });

    tooltip(
        button,
        container(text(label).size(12))
            .padding([4, 8])
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
}

/// A square button `size` wide showing `icon` (one of the icons above) in
/// the color of its text, styled by `style` and explained by `hint` on hover.
/// It sends `on_press`, and is disabled without one. Its icon has the widget
/// `id`, since there is no text to find it by.
///
/// The icon is drawn at the size it is designed at, to stay sharp, in
/// buttons with room for it; smaller buttons draw it at half their size.
pub fn icon_button<'a, Message: Clone + 'a>(
    id: &'static str,
    icon: &'static [u8],
    size: f32,
    style: fn(&Theme, button::Status) -> button::Style,
    hint: &'a str,
    on_press: Option<Message>,
) -> impl Widget<Message> + 'a {
    let status = if on_press.is_some() {
        button::Status::Active
    } else {
        button::Status::Disabled
    };

    let icon_size = if size >= ICON * 1.5 { ICON } else { size / 2.0 };

    let icon = svg(svg::Handle::from_memory(icon))
        .width(icon_size)
        .height(icon_size)
        .style(move |theme: &Theme, _status| svg::Style {
            color: Some(style(theme, status).text_color),
        });

    tooltip(
        button(center(icon).id(id))
            .width(size)
            .height(size)
            .padding(0)
            .style(style)
            .on_press_maybe(on_press),
        container(text(hint).size(12))
            .padding([4, 8])
            .style(container::rounded_box),
        tooltip::Position::Bottom,
    )
}

/// Shows `dialog`, if any, centered over a dimmed `base`; clicking outside it
/// sends `on_blur`. While it shows, the mouse doesn't reach `base`: neither
/// its clicks nor its wheel.
///
/// `base` sits in the same place whether a dialog is open or not, so it keeps
/// its widget state (scroll positions, focus) while dialogs come and go.
pub fn modal<'a, Message: Clone + 'a>(
    base: impl Widget<Message> + 'a,
    dialog: Option<impl Widget<Message> + 'a>,
    on_blur: Message,
) -> impl Widget<Message> + 'a {
    stack![
        base,
        dialog.map(|dialog| {
            backdrop(
                mouse_area(center(opaque(dialog)).style(|_theme| {
                    container::Style {
                        background: Some(
                            Color {
                                a: 0.7,
                                ..Color::BLACK
                            }
                            .into(),
                        ),
                        ..container::Style::default()
                    }
                }))
                .on_press(on_blur),
            )
        }),
    ]
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use iced::mouse::ScrollDelta;
    use iced::time::Instant;
    use iced::widget::scrollable;
    use iced::{Event, Fill, Point, window};
    use iced_test::simulator;

    use super::*;

    #[derive(Debug, Clone, PartialEq)]
    enum Scrolled {
        /// The list says how far down it is, whenever it is told it, moved
        /// or not.
        List(f32),
        Dialog,
        Blur,
    }

    /// Whether scrolling at `at` scrolls a long list filling the window,
    /// and whether it scrolls a `dialog`, when there is one, in the middle
    /// (of a 1024 by 768 window). A list scrolls with a levitating cursor
    /// too, as tooltips leave it: what the dialog is shown over must keep
    /// the wheel.
    fn scrolled(dialog: bool, at: Point) -> (bool, bool) {
        let list = scrollable(container(text("the list")).width(Fill).height(4000))
            .on_scroll(|scroll| Scrolled::List(scroll.viewport.absolute_offset().y));
        let dialog = dialog.then(|| {
            mouse_area(container(text("the dialog")).width(200).height(100))
                .on_scroll(|_| Scrolled::Dialog)
        });

        let mut ui = simulator(modal(list, dialog, Scrolled::Blur));
        ui.point_at(at);
        let _ = ui.scroll(ScrollDelta::Lines { x: 0.0, y: -3.0 });
        // A scroll may glide over a few frames.
        let start = Instant::now();
        let _ = ui.simulate((1..=10).map(|frame| {
            Event::Window(window::Event::RedrawRequested(
                start + Duration::from_millis(frame * 50),
            ))
        }));

        let messages: Vec<Scrolled> = ui.into_messages().collect();
        let list = messages
            .iter()
            .any(|message| matches!(message, Scrolled::List(down) if *down > 0.0));

        (list, messages.contains(&Scrolled::Dialog))
    }

    #[test]
    fn the_wheel_scrolls_the_dialog_and_never_what_is_under_it() {
        let (corner, middle) = (Point::new(5.0, 5.0), Point::new(512.0, 384.0));

        assert_eq!(scrolled(false, corner), (true, false));
        assert_eq!(scrolled(true, corner), (false, false), "beside the dialog");
        assert_eq!(scrolled(true, middle), (false, true), "over the dialog");
    }
}
