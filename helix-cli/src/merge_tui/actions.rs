#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Quit,
    Cancel,
    MoveUp,
    MoveDown,
    NextConflict,
    PrevConflict,
    TakeTarget,
    TakeSandbox,
    TakeBase,
    TakeBoth,
    ToggleExpand,
    Confirm,
    ToggleHelp,
}
