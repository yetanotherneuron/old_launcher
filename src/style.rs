//! The launcher's look: a dark theme with the gold of the WotLK interface,
//! and the few components every screen shares.

use iced::theme::palette;
use iced::widget::{button, center, container, mouse_area, opaque, stack, svg, text, tooltip};
use iced::{Border, Color, Fit, Font, Theme, Widget, border, color, font};

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

/// The default font in bold, for titles.
pub const BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

/// The palette a [`badge`] takes its color from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// Gold: facts about the addon.
    Primary,
    /// Green: all good.
    Success,
    /// Amber: works, but worth a look.
    Warning,
    /// Red: the client will not load it.
    Danger,
}

/// A dark, warm theme with gold as the primary color.
pub fn theme() -> Theme {
    Theme::custom(
        "Old Launcher",
        palette::Seed {
            background: color!(0x16141a),
            text: color!(0xe9e3d3),
            primary: color!(0xd4a64a),
            success: color!(0x5fb971),
            warning: color!(0xe3a33b),
            danger: color!(0xd65a52),
        },
    )
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
/// sends `on_blur`.
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
            opaque(
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
