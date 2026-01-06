#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    Cancel,

    // Conflict navigation
    NextConflict,
    PrevConflict,
    NextUnresolved,
    PrevUnresolved,

    // Diff scrolling
    ScrollDiffUp,
    ScrollDiffDown,
    ScrollDiffPageUp,
    ScrollDiffPageDown,
    ScrollDiffTop,
    ScrollDiffBottom,

    // Resolution
    TakeTarget,
    TakeSandbox,
    TakeBase,
    TakeBoth,

    // Other
    ToggleExpand,
    Confirm,
    ToggleHelp,
}
