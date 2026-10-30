//! The launcher's look: the game's login screens', as far as their colors,
//! type and shapes go (`docs/plans/wotlk-look.md` says what is taken and
//! what can't be), and the few components every screen shares.
//!
//! The colors are read off the game's own textures and `GlueXML`: dark
//! frames edged in grey metal with a knob at each corner, blue bevelled
//! buttons (red for what deletes, as the game's panel button is), grey
//! metal checkboxes ticked in yellow, blue-black fields, gold headings.

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

/// The knob at a frame's corner, drawn in the spirit of the game's dialog
/// frames (a diamond of dark metal where the edge's rods meet), which
/// [`ornamented`] lays on the corners of a dialog. Symmetric, so one
/// drawing serves all four (the screenshots' renderer doesn't turn an
/// SVG anyway).
static KNOB: Art = Art::new(include_bytes!("../assets/knob.svg"));

/// The size the knob is drawn at: the one it is designed at.
const KNOB_SIZE: f32 = 16.0;

/// A trash can, drawn in black like every icon: the buttons tint them.
pub static TRASH: Art = Art::new(include_bytes!("../assets/trash.svg"));

/// A plus sign, for adding.
pub static PLUS: Art = Art::new(include_bytes!("../assets/plus.svg"));

/// A pen, for editing.
pub static PEN: Art = Art::new(include_bytes!("../assets/pen.svg"));

/// Two curved arrows in a circle, for reading again.
pub static REFRESH: Art = Art::new(include_bytes!("../assets/refresh.svg"));

/// A cog, for the settings.
pub static COG: Art = Art::new(include_bytes!("../assets/cog.svg"));

/// A folder, for opening one in the file manager.
pub static FOLDER: Art = Art::new(include_bytes!("../assets/folder.svg"));

/// The size the icons are designed at: there, their lines fall on whole
/// pixels, which any smaller size smears across two or three.
const ICON: f32 = 24.0;

/// Fira Sans 4.203, under the SIL Open Font License
/// (`assets/fonts/FiraSans-OFL.txt`), for the body: the regular weight is
/// the very file iced brings, the bold Google Fonts' build of the same
/// release.
const FIRA_SANS: &[u8] = include_bytes!("../assets/fonts/FiraSans-Regular.ttf");
const FIRA_SANS_BOLD: &[u8] = include_bytes!("../assets/fonts/FiraSans-Bold.ttf");

/// Cinzel, under the SIL Open Font License (`assets/fonts/Cinzel-OFL.txt`):
/// capitals cut the way the game's headings are. The game's own fonts are
/// licensed, and stay its.
const CINZEL: &[u8] = include_bytes!("../assets/fonts/Cinzel-Regular.ttf");
const CINZEL_BOLD: &[u8] = include_bytes!("../assets/fonts/Cinzel-Bold.ttf");

/// The fonts the launcher brings, loaded before anything is drawn (and by
/// the screenshots): Fira Sans for the body, Cinzel for titles.
pub fn fonts() -> [&'static [u8]; 4] {
    [FIRA_SANS, FIRA_SANS_BOLD, CINZEL, CINZEL_BOLD]
}

/// The font of the text, unless it says otherwise: Fira Sans, which the
/// launcher brings. Where Fira Sans is installed, that copy draws it
/// instead: the system's fonts are loaded first, and of two fonts of the
/// same name and weight the text engine takes the first.
pub const FONT: Font = Font::new("Fira Sans");

/// The text's font in bold, for names and whatever stands out in a line.
pub const BOLD: Font = FONT.weight(font::Weight::Bold);

/// The font of titles and headings: Cinzel, bold, whose lowercase is small
/// capitals.
pub const TITLE: Font = Font {
    family: font::Family::Name("Cinzel"),
    weight: font::Weight::Bold,
    ..Font::DEFAULT
};

/// The colors of the game's login screens, read off its `GlueXML` and the
/// textures it draws them with, and the item qualities, which the badges
/// take.
pub mod wotlk {
    use iced::{Color, color};

    /// `GlueFontNormal`: headings, labels, buttons' text. (In play the
    /// game's gold is a shade lighter, `#ffd100`.)
    pub const GOLD: Color = color!(0xffc700);
    /// `GlueFontHighlight`: the text itself.
    pub const WHITE: Color = color!(0xf5f5f5);
    /// `GlueFontDisable`: disabled, and what is said in passing.
    pub const GREY: Color = color!(0x808080);
    /// `GlueFontRed`: errors.
    pub const RED: Color = color!(0xff1919);
    /// The near-black navy the window is filled with, over which the text
    /// is read: there is no world behind the launcher.
    pub const NAVY: Color = color!(0x0b0b14);
    /// The dialogs' fill (`UI-DialogBox-Background`): black, six tenths
    /// through, which over the window comes to this.
    pub const FRAME: Color = color!(0x050508);
    /// The boxes inset in a frame, below it.
    pub const INSET: Color = color!(0x030305);
    /// The fields' fill (`Glues-EditBox-Background`): blue-black.
    pub const FIELD: Color = color!(0x0a0e14);
    /// The fields' rim, and lit.
    pub const FIELD_EDGE: Color = color!(0x5c6a70);
    pub const FIELD_EDGE_LIT: Color = color!(0xa8b8c0);
    /// The dialogs' edge (`UI-DialogBox-Border`): a rod of grey metal, its
    /// lit side, its shadow and its outline.
    pub const METAL: Color = color!(0x686460);
    pub const METAL_SHADOW: Color = color!(0x353230);
    pub const METAL_DARK: Color = color!(0x1a1918);
    /// The light rim of the headers and tooltips.
    pub const METAL_LIGHT: Color = color!(0x989490);
    /// The login screens' button (`Glue-Panel-Button-Up-Blue`): blue,
    /// dark at the top, bright through the middle, dark at the bottom.
    pub const BLUE_TOP: Color = color!(0x15406d);
    pub const BLUE: Color = color!(0x12558d);
    pub const BLUE_BOTTOM: Color = color!(0x184070);
    /// The blue glow a checkbox has under the mouse
    /// (`UI-CheckBox-Highlight`).
    pub const BLUE_GLOW: Color = color!(0x2a4a90);
    /// The panel button of the game's dialogs (`UI-Panel-Button-Up`):
    /// crimson, lit through the middle.
    pub const CRIMSON_TOP: Color = color!(0x500000);
    pub const CRIMSON: Color = color!(0x680808);
    pub const CRIMSON_BOTTOM: Color = color!(0x400000);
    /// A button that does nothing (`Glue-Panel-Button-Disabled`), and the
    /// steel of the lesser buttons.
    pub const DISABLED: Color = color!(0x202020);
    pub const STEEL_TOP: Color = color!(0x323236);
    pub const STEEL_BOTTOM: Color = color!(0x1c1c20);
    /// The checkbox's tick (`UI-CheckBox-Check`): yellow.
    pub const TICK: Color = color!(0xf8f020);
    /// The dropdown's arrow (`UI-ChatIcon-ScrollDown-Up`): dim gold.
    pub const ARROW: Color = color!(0xd0c050);
    /// The scrollbar's knob (`UI-ScrollBar-Knob`): brown-grey metal.
    pub const KNOB: Color = color!(0x504840);
    /// A tooltip's fill and edge: blue-black, with a thin light rim.
    pub const TOOLTIP: Color = color!(0x0a0a1e);
    pub const TOOLTIP_EDGE: Color = color!(0x8c8c8c);

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

/// A frame, as the game draws its dialogs: filled near black, with a
/// square edge of grey metal. For list items and dialogs; a dialog gets
/// its corners from [`ornamented`].
pub fn card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(wotlk::FRAME.into()),
        border: border::rounded(0).color(wotlk::METAL).width(2),
        ..container::Style::default()
    }
}

/// A box inset in a frame, sunk below it: a list, an error, what a dialog
/// is about.
pub fn inset(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(wotlk::INSET.into()),
        border: border::rounded(0).color(wotlk::METAL_DARK).width(1),
        ..container::Style::default()
    }
}

/// A tooltip's box: the game's, blue-black with a thin light rim.
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

/// `frame`, a [`card`], with the game's knobs on its corners: a dialog.
/// The knobs sit centered on the corners, so the frame is padded by half
/// a knob for them to overhang into; the stack takes that size.
pub fn ornamented<'a, Message: 'a>(frame: impl Widget<Message> + 'a) -> impl Widget<Message> + 'a {
    let knob = |x, y| {
        container(svg(KNOB.handle()).width(KNOB_SIZE).height(KNOB_SIZE))
            .width(Fill)
            .height(Fill)
            .align_x(x)
            .align_y(y)
    };

    stack![
        container(frame).padding(KNOB_SIZE / 2.0),
        knob(Left, Top),
        knob(Right, Top),
        knob(Right, Bottom),
        knob(Left, Bottom),
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

/// A gradient from `top` to `bottom`, as the game shades its bars.
fn shaded(top: Color, bottom: Color) -> Background {
    Background::Gradient(
        Linear::new(Degrees(180.0))
            .add_stop(0.0, top)
            .add_stop(1.0, bottom)
            .into(),
    )
}

/// A gradient dark at the `top`, bright through the `middle` and dark at
/// the `bottom`: the glass of the game's buttons.
fn glossy(top: Color, middle: Color, bottom: Color) -> Background {
    Background::Gradient(
        Linear::new(Degrees(180.0))
            .add_stop(0.0, top)
            .add_stop(0.45, middle)
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

/// A button as the game draws them (`Glue-Panel-Button`): a glass of
/// `top`, `middle` and `bottom` in a bevel of grey metal with gold text,
/// lit under the mouse, darker held down, and flat dark grey with grey
/// text when it does nothing.
fn panel(status: button::Status, top: Color, middle: Color, bottom: Color) -> button::Style {
    let (face, edge, text) = match status {
        button::Status::Active => (glossy(top, middle, bottom), wotlk::METAL, wotlk::GOLD),
        button::Status::Hovered => (
            glossy(lit(top), lit(middle), lit(bottom)),
            lit(wotlk::METAL),
            lit(wotlk::GOLD),
        ),
        button::Status::Pressed => (
            glossy(dimmed(top), dimmed(middle), dimmed(bottom)),
            wotlk::METAL_SHADOW,
            wotlk::GOLD,
        ),
        button::Status::Disabled => (
            Background::Color(wotlk::DISABLED),
            wotlk::METAL_SHADOW,
            wotlk::GREY,
        ),
    };

    button::Style {
        background: Some(face),
        text_color: text,
        border: border::rounded(0).color(edge).width(1),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The login screen's blue button: what a dialog is for, and Play.
pub fn primary(_theme: &Theme, status: button::Status) -> button::Style {
    panel(status, wotlk::BLUE_TOP, wotlk::BLUE, wotlk::BLUE_BOTTOM)
}

/// A steel button: the other things to do.
pub fn secondary(_theme: &Theme, status: button::Status) -> button::Style {
    panel(
        status,
        wotlk::STEEL_BOTTOM,
        wotlk::STEEL_TOP,
        wotlk::STEEL_BOTTOM,
    )
}

/// The game's crimson panel button: what deletes.
pub fn danger(_theme: &Theme, status: button::Status) -> button::Style {
    panel(
        status,
        wotlk::CRIMSON_TOP,
        wotlk::CRIMSON,
        wotlk::CRIMSON_BOTTOM,
    )
}

/// A button that is only its gold text, lit under the mouse: closing,
/// dismissing, links.
pub fn quiet(_theme: &Theme, status: button::Status) -> button::Style {
    bare(wotlk::GOLD, status)
}

/// A link that is only its white text, lit under the mouse: beside a gold
/// name, so as not to pass for it.
pub fn link(_theme: &Theme, status: button::Status) -> button::Style {
    bare(wotlk::WHITE, status)
}

/// A button that is only its text, in `color`, lit under the mouse.
fn bare(color: Color, status: button::Status) -> button::Style {
    let (fill, text) = match status {
        button::Status::Active => (None, color),
        button::Status::Hovered => (Some(Color { a: 0.12, ..color }), lit(color)),
        button::Status::Pressed => (Some(Color { a: 0.2, ..color }), color),
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

/// The game's checkbox (`UI-CheckBox`): a dark square in grey metal,
/// ticked in yellow, glowing blue under the mouse.
pub fn check(_theme: &Theme, status: checkbox::Status) -> checkbox::Style {
    let (edge, tick, text) = match status {
        checkbox::Status::Active { .. } => (wotlk::METAL, wotlk::TICK, None),
        checkbox::Status::Hovered { .. } => (wotlk::BLUE_GLOW, lit(wotlk::TICK), None),
        checkbox::Status::Disabled { .. } => (wotlk::METAL_SHADOW, wotlk::GREY, Some(wotlk::GREY)),
    };

    checkbox::Style {
        background: wotlk::FRAME.into(),
        icon_color: tick,
        border: border::rounded(2).color(edge).width(1),
        text_color: text,
    }
}

/// The game's dropdown: a dark box in grey metal with a dim gold arrow,
/// its edge lit under the mouse.
pub fn picker(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let edge = match status {
        pick_list::Status::Active => wotlk::METAL_SHADOW,
        pick_list::Status::Hovered | pick_list::Status::Opened { .. } => wotlk::METAL,
        pick_list::Status::Disabled => wotlk::METAL_DARK,
    };
    let (text, handle) = match status {
        pick_list::Status::Disabled => (wotlk::GREY, wotlk::GREY),
        _ => (wotlk::WHITE, wotlk::ARROW),
    };

    pick_list::Style {
        text_color: text,
        placeholder_color: wotlk::GREY,
        handle_color: handle,
        background: wotlk::FRAME.into(),
        border: border::rounded(0).color(edge).width(1),
    }
}

/// What a dropdown opens: a dialog's box, the choice under the mouse lit
/// blue, as the game's dropdowns light theirs.
pub fn choices(_theme: &Theme) -> menu::Style {
    menu::Style {
        background: wotlk::FRAME.into(),
        border: border::rounded(0).color(wotlk::METAL).width(1),
        text_color: wotlk::WHITE,
        selected_text_color: wotlk::GOLD,
        selected_background: Color {
            a: 0.6,
            ..wotlk::BLUE_GLOW
        }
        .into(),
        shadow: Shadow::default(),
    }
}

/// The login screen's text field: blue-black, with a light rim while it
/// has the focus.
pub fn field(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let edge = match status {
        text_input::Status::Active => wotlk::FIELD_EDGE,
        text_input::Status::Disabled => wotlk::METAL_SHADOW,
        text_input::Status::Hovered => lit(wotlk::FIELD_EDGE),
        text_input::Status::Focused { .. } => wotlk::FIELD_EDGE_LIT,
    };
    let value = match status {
        text_input::Status::Disabled => wotlk::GREY,
        _ => wotlk::WHITE,
    };

    text_input::Style {
        background: wotlk::FIELD.into(),
        border: border::rounded(0).color(edge).width(1),
        placeholder: wotlk::GREY,
        value,
        selection: Color {
            a: 0.5,
            ..wotlk::BLUE
        },
    }
}

/// The game's scrollbar: a thin dark rail with a knob of brown-grey
/// metal, lit under the mouse.
pub fn scroll(_theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let rail = |knob: Color| scrollable::Rail {
        background: Some(wotlk::INSET.into()),
        border: border::rounded(0).color(wotlk::METAL_DARK).width(1),
        scroller: scrollable::Scroller {
            background: knob.into(),
            border: border::rounded(2).color(wotlk::METAL_DARK).width(1),
        },
    };
    let (vertical, horizontal) = match status {
        scrollable::Status::Active { .. } => (rail(wotlk::KNOB), rail(wotlk::KNOB)),
        scrollable::Status::Hovered {
            is_vertical_scrollbar_hovered,
            is_horizontal_scrollbar_hovered,
            ..
        } => (
            rail(if is_vertical_scrollbar_hovered {
                wotlk::METAL
            } else {
                wotlk::KNOB
            }),
            rail(if is_horizontal_scrollbar_hovered {
                wotlk::METAL
            } else {
                wotlk::KNOB
            }),
        ),
        scrollable::Status::Dragged { .. } => (rail(wotlk::METAL_LIGHT), rail(wotlk::METAL_LIGHT)),
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
            border: border::rounded(u32::MAX).width(1).color(wotlk::METAL),
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

/// The login screen's loading bar: a sunk track in grey metal, filled in
/// shaded blue (what its glow is).
pub fn bar(_theme: &Theme) -> progress_bar::Style {
    progress_bar::Style {
        background: wotlk::INSET.into(),
        bar: shaded(lit(wotlk::BLUE), wotlk::BLUE_BOTTOM),
        border: border::rounded(0).color(wotlk::METAL_SHADOW).width(1),
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
