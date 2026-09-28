//! What a screen's `update` hands back to its parent.

use iced::Task;

/// A screen's reaction to a message: work for the runtime (`task`) and,
/// optionally, a decision only the parent can make (`instruction`).
#[must_use = "Action.task must be returned to iced; Action.instruction must be matched by the parent"]
pub struct Action<I, M> {
    /// A decision for the parent.
    pub instruction: Option<I>,
    /// Work for the runtime, producing the screen's own messages.
    pub task: Task<M>,
}

impl<I, M> Action<I, M> {
    /// Nothing to do.
    pub fn none() -> Self {
        Self {
            instruction: None,
            task: Task::none(),
        }
    }

    /// Only an instruction for the parent.
    pub fn instruction(instruction: I) -> Self {
        Self {
            instruction: Some(instruction),
            task: Task::none(),
        }
    }

    /// Only a task for the runtime.
    pub fn task(task: Task<M>) -> Self {
        Self {
            instruction: None,
            task,
        }
    }

    /// An instruction for the parent, and a task for the runtime.
    pub fn new(instruction: I, task: Task<M>) -> Self {
        Self {
            instruction: Some(instruction),
            task,
        }
    }
}

impl<I, M> From<Task<M>> for Action<I, M> {
    fn from(task: Task<M>) -> Self {
        Self::task(task)
    }
}
