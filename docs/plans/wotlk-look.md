# A look like the game's

Status: built on 2026-10-26, after 0.11.0, with these calls made: Cinzel
for titles and Fira Sans for the body; the badges in the item-quality
colors; square corners with ornaments; all four phases. First built from
memory and the `FrameXML` constants, which got the buttons and frames
wrong (gold edges and red buttons: a loot window); then corrected against
the game's own textures, read from a client's locale archives, and its
`GlueXML`, to the login screens' look, which is where the launcher sits.
The table below says what the textures show. DEVELOPMENT.md ("The look")
says how it is.

## The ask

The launcher should feel like the game's own interface, with the colors of
Wrath of the Lich King: its dark, see-through frames with gold edges, its
gold headings and white text, its red-and-gold buttons. The layout and what
every screen does stay as they are: this is a skin, not a redesign of the
flows.

## What the game's interface is made of

Read off the 3.3.5a textures (`Interface\Glues\Common`,
`Interface\DialogFrame`, `Interface\Buttons` in the locale archives) and
the `GlueXML` that uses them on the login, realm and character screens.
Nothing of it can be copied (below), but all of it can be drawn in the
same spirit.

**Colors** (`GlueXML/GlueFontStyles.xml` for the text; the rest sampled
from the textures):

| Role | The game's | Where it goes here |
|---|---|---|
| Headings, labels, "normal" text | gold `#ffc700` (`GlueFontNormal`; in play `#ffd100`) | titles, section headings, facts' labels, button text |
| Body, highlighted text | white `#ffffff` | the rest of the text |
| Disabled, secondary | grey `#808080` | secondary text, disabled buttons |
| Errors | red `#ff1919` | the danger tone |
| Good | green `#19ff19` | the success tone |
| Dialog backgrounds | black at 0.6 (`UI-DialogBox-Background`) over the world | cards, dialogs |
| Dialog borders | a rod of grey metal (`#686460` lit, `#353230` in shadow, `#1a1918` outline), with a diamond knob at each corner (`UI-DialogBox-Border`) | cards' edges; the knobs on dialogs |
| Buttons on the login screens (`GlueButtonTemplateBlue`) | blue glass, dark at the top (`#15406d`), bright through the middle (`#12558d`), dark at the bottom (`#184070`), in a grey-metal bevel; brighter hovered; flat `#202020` with grey text disabled | primary buttons, Play |
| Buttons in the game's dialogs (`UI-Panel-Button-Up`) | crimson glass (`#680808`) in the same bevel, gold text | the danger buttons |
| Checkboxes (`UI-CheckBox-Up`) | a dark square in grey metal, a yellow tick (`#f8f020`), a blue glow hovered | checkboxes |
| Fields (`Glues-EditBox-Background`) | blue-black (`#0a0e14`) with a grey-blue rim (`#7d8a90`, `#a8b8c0` lit) | text fields |
| Dropdowns | a dark box in grey metal with a dim gold arrow (`#d0c050`) | pick lists |
| Scrollbars | a thin dark rail, a brown-grey metal knob (`#504840`) | scrollbars |
| Tooltip background | black at 0.67 tinted blue by the code, with a thin light-grey rim | tooltips |
| Loading bar | its fill isn't in the locale archives; its glow is blue | progress bars, in the buttons' blue |
| Gold frames (`UI-DialogBox-Gold-*`) | the loot window's and the dragon-headed panels' only | nowhere: gold is for text and the tick |
| Item quality, for badges | poor `#9d9d9d`, common `#ffffff`, uncommon `#1eff00`, rare `#0070dd`, epic `#a335ee`, legendary `#ff8000` | the badge tones: "Installed" uncommon green, "Update available" rare blue, "Modified" legendary orange, "No longer available" poor grey |

**Shapes:** frames are square-cornered with a beveled metal edge and a
knob at each corner; buttons are wide and short glass in a metal bevel;
checkboxes are metal squares with a yellow check; dropdowns are dark with
a gold arrow; scrollbars thin, with a metal knob.

**Type:** titles and most of the interface in Friz Quadrata (flared
serifs), quest text in Morpheus, chat in Arial Narrow, damage in Skurri.

## What can't be used, and what can

- **None of Blizzard's files.** The textures (`.blp` in the MPQs), the
  fonts (`FRIZQT__.TTF`, `MORPHEUS.TTF`, `ARIALN.TTF`, `SKURRI.TTF`) and
  the art are all Blizzard's or licensed to it. Nothing is extracted from
  a client or copied from a screenshot; the look is reproduced from its
  colors and shapes, which aren't anyone's.
- **Fonts under the SIL Open Font License**, bundled in the binary as
  iced's Fira Sans is now, so every system (and the screenshot container)
  draws the same thing. No open font copies Friz Quadrata; candidates with
  the same feel, to be judged on renders:
  - titles: Cinzel (inscriptional capitals, the feel of the game's zone
    names and Morpheus headings) or Marcellus (flared, closer to Friz
    Quadrata's lowercase);
  - body: keep Fira Sans (what the launcher draws now, and legible), or
    Liberation Sans Narrow, which is metric-compatible with Arial Narrow,
    the game's chat font. A narrow body font fits more on each card.
- **Own art only:** frame corners and edges drawn as SVG by us, in the
  game's spirit (gold bevels, a small flourish), the way the four icons in
  `assets/` are.

## What iced gives us (master, `cd04b74`)

Checked in the checkout:

- `Theme::custom` with a `palette::Seed` is what `style::theme()` uses:
  colors only. Everything else is per-widget styling.
- `container::Style` and `button::Style` take a background that is a color
  or a **linear gradient** (`Background::Gradient`, up to 8 stops), a
  `Border` (one color, width and radius for all four sides) and a
  `Shadow`. That is enough for beveled gold edges: a gold outer container
  holding a dark inner one with a 1 px darker-gold inset line, and a
  gradient for the button face.
- `svg` is enabled: vector corner ornaments and edge strips, tinted
  through `svg::Style`, laid over a frame with `stack`. There is no
  nine-slice widget; a frame is a stack of the filled container, four
  edge strips and four corners, which a `frame()` helper in `style.rs`
  hides.
- `image` and `canvas` are available features but not enabled; neither is
  needed. `image` would bring the `image` crate for raster art; `canvas`
  could draw ornaments procedurally. Keep both off unless the SVG route
  proves too limited.
- Custom fonts: `iced::font::load(bytes)` at start, or `Settings::fonts`;
  `Font::with_name("Cinzel")` then selects them. Bundled with
  `include_bytes!`, like the icons.

## Phases

Each phase is a commit on its own that leaves the launcher whole, with the
screenshots rendered again (the container renders need the fonts bundled,
which they will be).

1. **Palette and type.** The WotLK colors as constants in `style.rs`, the
   `Seed` built from them, titles in the chosen title font in gold, body in
   the chosen body font in white, secondary text grey. The tones mapped to
   the game's colors (and item-quality colors for the badges, if wanted).
   Small, and already most of the feel.
2. **Frames.** `card()` becomes the game's frame: dark navy translucent
   fill (over the window's own dark background, since there is no world
   behind), beveled gold edge, square corners with SVG ornaments; dialogs
   the same, larger; tooltips the tooltip backdrop; banners keep their tone
   tint inside such a frame.
3. **Controls.** Buttons as `UIPanelButton` (red-brown gradient, gold
   frame and text, brighter hovered, grey text disabled; the primary and
   secondary styles differ by the fill's warmth), gold checkboxes and
   pick-list arrows, dark inset text fields with a gold focus edge, a thin
   gold scrollbar, status-bar progress bars.
4. **Finishing.** The header as the game's title (gold capitals), the Play
   button as the login screen's big red-and-gold one, the first-run form
   as a glue-screen dialog; a faint drawn pattern for the window
   background if flat navy looks empty. README screenshots and a line.

## Decided when the plan is taken up

- Which title font and which body font (renders of both candidates first).
- Whether the badges take the item-quality colors or stay as they are.
- Square corners with ornaments (the game's) or the current rounded ones
  with only the gold edge.
- How far to go: phases 1–2 give the look; 3–4 are polish.
