#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Quit,
    MoveUp,
    MoveDown,
    PageUp,
    PageDown,
    GoToTop,
    GoToBottom,
    ToggleStage,
    StageAll,
    UnstageAll,
    Refresh,
    ToggleUntracked,
    ToggleHelp,
    SwitchSection,   // Tab
    CollapseSection, // h
    ExpandSection,   // l
}
