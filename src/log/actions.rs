#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    MoveUp,
    MoveDown,
    PageUp,   // page up (10 commits)
    PageDown, // page down (10 commits)
    GoToTop,
    GoToBottom,
    CheckoutCommit,
    EnterSearchMode,
    // EnterVimMode,
    ExitSearchMode,
}
