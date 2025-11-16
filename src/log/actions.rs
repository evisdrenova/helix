#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    MoveUp,
    MoveDown,
    // page up (10 commits)
    PageUp,
    // page down (10 commits)
    PageDown,
    GoToTop,
    GoToBottom,
    CheckoutCommit,
}
