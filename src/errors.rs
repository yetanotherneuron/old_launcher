//! Problems the launcher ran into and got past, shown in one banner until
//! dismissed.

use iced::widget::{button, column, container, row, text};
use iced::{Center, Fill, Widget};

use crate::style::{self, Tone};

/// The problems waiting to be seen, oldest first.
#[derive(Debug, Default)]
pub struct Errors(Vec<String>);

impl Errors {
    /// Adds `error` to the banner, unless it already says so. It goes to the
    /// log too, for whoever reads it.
    pub fn report(&mut self, error: impl Into<String>) {
        let error = error.into();
        log::warn!("{error}");

        if !self.0.contains(&error) {
            self.0.push(error);
        }
    }

    /// Empties the banner.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// The problems, oldest first.
    #[cfg(test)]
    pub fn messages(&self) -> &[String] {
        &self.0
    }

    /// The banner, when there is anything to show; `dismiss` clears it.
    pub fn view<'a, Message: Clone + 'a>(
        &'a self,
        dismiss: Message,
    ) -> Option<impl Widget<Message> + 'a> {
        (!self.0.is_empty()).then(|| {
            container(
                row![
                    column(self.0.iter().map(text)).spacing(4).width(Fill),
                    button("Dismiss").style(style::quiet).on_press(dismiss),
                ]
                .spacing(8)
                .align_y(Center),
            )
            .padding([6, 12])
            .style(style::banner(Tone::Danger))
        })
    }
}
