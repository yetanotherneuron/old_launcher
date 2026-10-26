//! A bar for work that can't say how far it got: a stretch of it glides
//! from end to end and back while the work goes on, in the colors of a
//! progress bar.

use std::f32::consts::TAU;

use iced::advanced::layout::{self, Layout};
use iced::advanced::renderer::{self, Quad};
use iced::advanced::widget::{Meta, Tree, tree};
use iced::advanced::{Shell, Widget};
use iced::time::Instant;
use iced::widget::progress_bar;
use iced::{Border, Color, Event, Length, Rectangle, Size, Theme, mouse, window};

/// How long the stretch takes to reach the far end and come back, in
/// seconds.
const CYCLE: f32 = 2.4;

/// How much of the bar the stretch covers.
const STRETCH: f32 = 0.3;

/// A bar `length` long and `girth` thick that shows work goes on.
pub fn activity(length: f32, girth: f32) -> Activity {
    Activity { length, girth }
}

/// A bar that shows work goes on, without saying how far it got.
pub struct Activity {
    length: f32,
    girth: f32,
}

/// When the bar was first shown, and when it was last drawn.
struct State {
    shown: Instant,
    now: Instant,
}

impl Meta for Activity {}

impl<Message, Renderer> Widget<Message, Theme, Renderer> for Activity
where
    Renderer: renderer::Renderer,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        let now = Instant::now();
        tree::State::new(State { shown: now, now })
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.length), Length::Fixed(self.girth))
    }

    fn layout(&mut self, tree: &mut Tree, _renderer: &Renderer, limits: &layout::Limits) {
        tree.size = layout::atomic(limits, self.length, self.girth);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        _layout: Layout,
        _cursor: mouse::Cursor,
        _renderer: &Renderer,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event {
            tree.state.downcast_mut::<State>().now = *now;
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        _style: &renderer::Style,
        layout: Layout,
        _cursor: mouse::Cursor,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let style = progress_bar::primary(theme);
        let state = tree.state.downcast_ref::<State>();
        let seconds = state
            .now
            .saturating_duration_since(state.shown)
            .as_secs_f32();
        // From one end to the other and back, slowing down at each end.
        let along = (1.0 - (seconds / CYCLE * TAU).cos()) / 2.0;
        let width = bounds.width * STRETCH;

        renderer.fill_quad(
            Quad {
                bounds,
                border: style.border,
                ..Quad::default()
            },
            style.background,
        );
        renderer.fill_quad(
            Quad {
                bounds: Rectangle {
                    x: bounds.x + (bounds.width - width) * along,
                    width,
                    ..bounds
                },
                border: Border {
                    color: Color::TRANSPARENT,
                    ..style.border
                },
                ..Quad::default()
            },
            style.bar,
        );
    }
}
