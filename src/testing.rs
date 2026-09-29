//! Fixtures shared by the screens' tests.

use iced_test::core::widget::{Id, Operation, operation};
use iced_test::core::{Rectangle, Size, Vector};
use iced_test::simulator::Simulator;

pub use crate::core::testing::client;

/// The vertical scroll offset of every scrollable on screen, as drawn, in
/// the order they are laid out.
pub fn offsets<Message>(ui: &mut Simulator<'_, Message>) -> Vec<f32> {
    struct Offsets(Vec<f32>);

    impl Operation for Offsets {
        fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
            operate(self);
        }

        fn scrollable(
            &mut self,
            _id: Option<&Id>,
            _bounds: Rectangle,
            _content: Size,
            translation: Vector,
            _state: &mut dyn operation::Scrollable,
        ) {
            self.0.push(translation.y);
        }
    }

    // Scrollables report the translation they were last drawn with.
    ui.draw(&crate::style::theme());

    let mut offsets = Offsets(Vec::new());
    ui.operate(&mut offsets);
    offsets.0
}
