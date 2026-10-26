//! The launcher's look: the game's own, as far as its colors, type and
//! shapes go (`docs/plans/wotlk-look.md` says what is taken and what can't
//! be), and the few components every screen shares.

use iced::gradient::Linear;
use iced::theme::palette;
use iced::widget::overlay::menu;
use iced::widget::{
    button, center, checkbox, container, mouse_area, opaque, pick_list, progress_bar, scrollable,
    stack, svg, text, text_input, tooltip,
};
use iced::{
    Background, Bottom, Color, Degrees, Fill, Fit, Font, Left, Right, Shadow, Theme, Top, Vector,
    Widget, border, font,
};

use std::sync::OnceLock;

use crate::backdrop::backdrop;

/// An icon or ornament the launcher brings as an SVG. Its handle is made
/// once: the renderer caches the drawing by the handle's id, a hash of the
/// file, which making a handle each frame would hash again each frame.
pub struct Art {
    bytes: &'static [u8],
    handle: OnceLock<svg::Handle>,
}

impl Art {
    const fn new(bytes: &'static [u8]) -> Self {
        Self {
            bytes,
            handle: OnceLock::new(),
        }
    }

    /// The handle to draw it by.
    fn handle(&self) -> svg::Handle {
        self.handle
            .get_or_init(|| svg::Handle::from_memory(self.bytes))
            .clone()
    }
}

/// A frame's corners, drawn in the spirit of the game's dialog frames (a
/// gold bracket with a knot), which [`ornamented`] lays at the corners of
/// a dialog: the same drawing turned in each file, since the screenshots'
/// renderer doesn't turn an SVG.
static CORNERS: [Art; 4] = [
    Art::new(include_bytes!("../assets/corner-top-left.svg")),
    Art::new(include_bytes!("../assets/corner-top-right.svg")),
    Art::new(include_bytes!("../assets/corner-bottom-right.svg")),
    Art::new(include_bytes!("../assets/corner-bottom-left.svg")),
];

/// The size the corners are drawn at: the one they are designed at.
const CORNER_SIZE: f32 = 28.0;

/// A trash can, drawn in black like every icon: the buttons tint them.
pub static TRASH: Art = Art::new(include_bytes!("../assets/trash.svg"));

/// A plus sign, for adding.
pub static PLUS: Art = Art::new(include_bytes!("../assets/plus.svg"));

/// A pen, for editing.
pub static PEN: Art = Art::new(include_bytes!("../assets/pen.svg"));

/// A cog, for the settings.
pub static COG: Art = Art::new(include_bytes!("../assets/cog.svg"));

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
    /// `GRAY_FONT_COLOR`: disabled, and what is said in passing.
    pub const GREY: Color = color!(0x808080);
    /// `RED_FONT_COLOR`: errors.
    pub const RED: Color = color!(0xff1919);
    /// The near-black navy the window is filled with, over which the text
    /// is read.
    pub const NAVY: Color = color!(0x0b0b14);
    /// The frames' fill, a shade above the window's.
    pub const FRAME: Color = color!(0x11111e);
    /// The boxes inset in a frame, a shade below the window's.
    pub const INSET: Color = color!(0x07070e);
    /// The frames' gold edge, and its shadow.
    pub const EDGE: Color = color!(0xa88a35);
    pub const EDGE_SHADOW: Color = color!(0x4a3a14);
    /// A tooltip's fill and edge: the game's, blue-black and steel blue.
    pub const TOOLTIP: Color = color!(0x0a0a1e);
    pub const TOOLTIP_EDGE: Color = color!(0x4c4c73);

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

/// A frame, as the game draws its dialogs and panels: filled a shade above
/// the window, with a square gold edge. For list items and dialogs; a
/// dialog gets its corners from [`ornamented`].
pub fn card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(wotlk::FRAME.into()),
        border: border::rounded(0).color(wotlk::EDGE).width(2),
        ..container::Style::default()
    }
}

/// A box inset in a frame, sunk below the window: a list, an error, what
/// a dialog is about.
pub fn inset(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(wotlk::INSET.into()),
        border: border::rounded(0).color(wotlk::EDGE_SHADOW).width(1),
        ..container::Style::default()
    }
}

/// A tooltip's box: the game's, blue-black with a steel-blue edge.
pub fn tooltip_box(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(
            Color {
                a: 0.96,
                ..wotlk::TOOLTIP
            }
            .into(),
        ),
        border: border::rounded(0).color(wotlk::TOOLTIP_EDGE).width(1),
        ..container::Style::default()
    }
}

/// `frame`, a [`card`], with the game's corners laid over its own: a
/// dialog. The corners take the frame's size, which the stack keeps.
pub fn ornamented<'a, Message: 'a>(frame: impl Widget<Message> + 'a) -> impl Widget<Message> + 'a {
    let corner = |drawn: &'static Art, x, y| {
        container(svg(drawn.handle()).width(CORNER_SIZE).height(CORNER_SIZE))
            .width(Fill)
            .height(Fill)
            .align_x(x)
            .align_y(y)
    };

    stack![
        frame,
        corner(&CORNERS[0], Left, Top),
        corner(&CORNERS[1], Right, Top),
        corner(&CORNERS[2], Right, Bottom),
        corner(&CORNERS[3], Left, Bottom),
    ]
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
            .style(tooltip_box),
        tooltip::Position::Bottom,
    )
}

/// A gradient from `top` to `bottom`, as the game shades its buttons and
/// bars.
fn shaded(top: Color, bottom: Color) -> Background {
    Background::Gradient(
        Linear::new(Degrees(180.0))
            .add_stop(0.0, top)
            .add_stop(1.0, bottom)
            .into(),
    )
}

/// `color`, a little brighter: a button under the mouse.
fn lit(color: Color) -> Color {
    Color {
        r: (color.r + 0.12).min(1.0),
        g: (color.g + 0.12).min(1.0),
        b: (color.b + 0.12).min(1.0),
        a: color.a,
    }
}

/// `color`, a little darker: a button held down.
fn dimmed(color: Color) -> Color {
    Color {
        r: color.r * 0.8,
        g: color.g * 0.8,
        b: color.b * 0.8,
        a: color.a,
    }
}

/// A panel button as the game draws them (`UIPanelButtonTemplate`): shaded
/// from `top` to `bottom`, with an `edge` and `text` of its own, brighter
/// under the mouse, darker held down, and grey when it does nothing.
fn panel(
    status: button::Status,
    top: Color,
    bottom: Color,
    edge: Color,
    text: Color,
) -> button::Style {
    let (top, bottom, edge, text) = match status {
        button::Status::Active => (top, bottom, edge, text),
        button::Status::Hovered => (lit(top), lit(bottom), lit(edge), lit(text)),
        button::Status::Pressed => (dimmed(top), dimmed(bottom), edge, text),
        button::Status::Disabled => (wotlk::FRAME, wotlk::INSET, wotlk::EDGE_SHADOW, wotlk::GREY),
    };

    button::Style {
        background: Some(shaded(top, bottom)),
        text_color: text,
        border: border::rounded(0).color(edge).width(1),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The game's red button with gold text: what a dialog is for.
pub fn primary(_theme: &Theme, status: button::Status) -> button::Style {
    panel(
        status,
        Color::from_rgb8(0x7a, 0x1f, 0x1f),
        Color::from_rgb8(0x3d, 0x0f, 0x0f),
        wotlk::EDGE,
        wotlk::GOLD,
    )
}

/// A navy button with gold text: the other things to do.
pub fn secondary(_theme: &Theme, status: button::Status) -> button::Style {
    panel(
        status,
        Color::from_rgb8(0x2a, 0x2a, 0x44),
        Color::from_rgb8(0x14, 0x14, 0x1f),
        wotlk::EDGE_SHADOW,
        wotlk::GOLD,
    )
}

/// A deep red button with a red edge and white text: what deletes.
pub fn danger(_theme: &Theme, status: button::Status) -> button::Style {
    panel(
        status,
        Color::from_rgb8(0x8b, 0x1a, 0x1a),
        Color::from_rgb8(0x4a, 0x0d, 0x0d),
        wotlk::RED,
        wotlk::WHITE,
    )
}

/// A button that is only its gold text, lit under the mouse: closing,
/// dismissing, links.
pub fn quiet(_theme: &Theme, status: button::Status) -> button::Style {
    let (fill, text) = match status {
        button::Status::Active => (None, wotlk::GOLD),
        button::Status::Hovered => (
            Some(Color {
                a: 0.12,
                ..wotlk::GOLD
            }),
            lit(wotlk::GOLD),
        ),
        button::Status::Pressed => (
            Some(Color {
                a: 0.2,
                ..wotlk::GOLD
            }),
            wotlk::GOLD,
        ),
        button::Status::Disabled => (None, wotlk::GREY),
    };

    button::Style {
        background: fill.map(Background::Color),
        text_color: text,
        border: border::rounded(0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The game's checkbox: a sunk gold-edged square, ticked in gold.
pub fn check(_theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let (edge, tick, text) = match status {
        checkbox::Status::Active { .. } => (wotlk::EDGE, wotlk::GOLD, None),
        checkbox::Status::Hovered { .. } => (lit(wotlk::EDGE), lit(wotlk::GOLD), None),
        checkbox::Status::Disabled { .. } => (wotlk::EDGE_SHADOW, wotlk::GREY, Some(wotlk::GREY)),
    };

    checkbox::Style {
        background: wotlk::INSET.into(),
        icon_color: tick,
        border: border::rounded(0).color(edge).width(1),
        text_color: text,
    }
}

/// The game's dropdown: a sunk field with a gold arrow, its edge lit under
/// the mouse.
pub fn picker(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let edge = match status {
        pick_list::Status::Active => wotlk::EDGE_SHADOW,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => wotlk::EDGE,
        pick_list::Status::Disabled => wotlk::EDGE_SHADOW,
    };
    let (text, handle) = match status {
        pick_list::Status::Disabled => (wotlk::GREY, wotlk::GREY),
        _ => (wotlk::WHITE, wotlk::GOLD),
    };

    pick_list::Style {
        text_color: text,
        placeholder_color: wotlk::GREY,
        handle_color: handle,
        background: wotlk::INSET.into(),
        border: border::rounded(0).color(edge).width(1),
    }
}

/// What a dropdown opens: the tooltip's box, the choice under the mouse in
/// gold.
pub fn choices(_theme: &Theme) -> menu::Style {
    menu::Style {
        background: wotlk::TOOLTIP.into(),
        border: border::rounded(0).color(wotlk::EDGE).width(1),
        text_color: wotlk::WHITE,
        selected_text_color: wotlk::GOLD,
        selected_background: Color {
            a: 0.2,
            ..wotlk::GOLD
        }
        .into(),
        shadow: Shadow::default(),
    }
}

/// The game's text field: sunk, with a gold edge while it has the focus.
pub fn field(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let edge = match status {
        text_input::Status::Active | text_input::Status::Disabled => wotlk::EDGE_SHADOW,
        text_input::Status::Hovered => lit(wotlk::EDGE_SHADOW),
        text_input::Status::Focused { .. } => wotlk::EDGE,
    };
    let value = match status {
        text_input::Status::Disabled => wotlk::GREY,
        _ => wotlk::WHITE,
    };

    text_input::Style {
        background: wotlk::INSET.into(),
        border: border::rounded(0).color(edge).width(1),
        placeholder: wotlk::GREY,
        value,
        selection: Color {
            a: 0.35,
            ..wotlk::GOLD
        },
    }
}

/// The game's scrollbar: a thin sunk rail with a gold knob, lit under the
/// mouse.
pub fn scroll(_theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let rail = |knob: Color| scrollable::Rail {
        background: Some(wotlk::INSET.into()),
        border: border::rounded(0).color(wotlk::EDGE_SHADOW).width(1),
        scroller: scrollable::Scroller {
            background: knob.into(),
            border: border::rounded(0),
        },
    };
    let (vertical, horizontal) = match status {
        scrollable::Status::Active { .. } => (rail(wotlk::EDGE), rail(wotlk::EDGE)),
        scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered,
            is_horizontal_scrollbar_hovered,
            ..
        } => (
            rail(if is_vertical_scrollbar_hovered {
                lit(wotlk::EDGE)
            } else {
                wotlk::EDGE
            }),
            rail(if is_horizontal_scrollbar_hovered {
                lit(wotlk::EDGE)
            } else {
                wotlk::EDGE
            }),
        ),
        scrollable::Status::Dragged { .. } => (rail(wotlk::GOLD), rail(wotlk::GOLD)),
    };

    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: vertical,
        horizontal_rail: horizontal,
        gap: None,
        auto_scroll: scrollable::AutoScroll {
            background: Color {
                a: 0.9,
                ..wotlk::NAVY
            }
            .into(),
            border: border::rounded(u32::MAX).width(1).color(wotlk::EDGE),
            shadow: Shadow {
                color: Color {
                    a: 0.7,
                    ..Color::BLACK
                },
                offset: Vector::ZERO,
                blur_radius: 2.0,
            },
            icon: wotlk::GOLD,
        },
    }
}

/// The game's status bar: a sunk track filled in shaded gold.
pub fn bar(_theme: &Theme) -> progress_bar::Style {
    progress_bar::Style {
        background: wotlk::INSET.into(),
        bar: shaded(wotlk::GOLD, Color::from_rgb8(0xa8, 0x7a, 0x0b)),
        border: border::rounded(0).color(wotlk::EDGE_SHADOW).width(1),
    }
}

/// A full-width notice tinted with `tone`, whose text takes the tone's color.
pub fn banner(tone: Tone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let color = tone.color(theme);

        container::Style {
            background: Some(Color { a: 0.12, ..color }.into()),
            text_color: Some(color),
            border: border::rounded(0).color(Color { a: 0.5, ..color }).width(1),
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
                border: border::rounded(0).color(Color { a: 0.6, ..color }).width(1),
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
                border: border::rounded(0).color(Color { a: 0.6, ..color }).width(1),
                ..button::Style::default()
            }
        });

    tooltip(
        badge,
        container(text(url).size(12))
            .padding([4, 8])
            .style(tooltip_box),
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

    let icon = svg(TRASH.handle())
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
            .style(tooltip_box),
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
    icon: &'static Art,
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

    let icon = svg(icon.handle()).width(icon_size).height(icon_size).style(
        move |theme: &Theme, _status| svg::Style {
            color: Some(style(theme, status).text_color),
        },
    );

    tooltip(
        button(center(icon).id(id))
            .width(size)
            .height(size)
            .padding(0)
            .style(style)
            .on_press_maybe(on_press),
        container(text(hint).size(12))
            .padding([4, 8])
            .style(tooltip_box),
        tooltip::Position::Bottom,
    )
}

/// Shows `dialog`, if any, with the game's corners (see [`ornamented`]),
/// centered over a dimmed `base`; clicking outside it sends `on_blur`.
/// While it shows, the mouse doesn't reach `base`: neither its clicks nor
/// its wheel.
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
                mouse_area(center(opaque(ornamented(dialog))).style(|_theme| {
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
