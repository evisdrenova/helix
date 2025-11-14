// src/log/actions.rs
//
// User actions that can be performed in the TUI

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Quit the application
    Quit,
    
    /// Move selection up
    MoveUp,
    
    /// Move selection down
    MoveDown,
    
    /// Page up (10 commits)
    PageUp,
    
    /// Page down (10 commits)
    PageDown,
    
    /// Go to first commit
    GoToTop,
    
    /// Go to last commit
    GoToBottom,
    
    /// Adjust split pane ratio to the left (more timeline)
    AdjustSplitLeft,
    
    /// Adjust split pane ratio to the right (more details)
    AdjustSplitRight,
}
