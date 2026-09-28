//! Fixtures shared by the screens' tests.

use std::fs;

use iced_test::core::widget::{Id, Operation, operation};
use iced_test::core::{Rectangle, Size, Vector};
use iced_test::simulator::Simulator;

use crate::core::Installation;

/// A minimal 3.3.5a client in a temporary directory: a `Wow.exe` whose
/// version resource says 3.3.5.12340, plus empty `files` (relative paths).
pub fn client(files: &[&str]) -> (tempfile::TempDir, Installation) {
    let root = tempfile::tempdir().unwrap();
    let mut exe = b"MZ".to_vec();
    for word in [0xFEEF_04BD_u32, 0x0001_0000, 3 << 16 | 3, 5 << 16 | 12340] {
        exe.extend(word.to_le_bytes());
    }
    fs::write(root.path().join("Wow.exe"), exe).unwrap();

    for file in files {
        let path = root.path().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
    }

    let installation = Installation::open(root.path()).unwrap();
    (root, installation)
}

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
