// src/log/mod.rs
//
// Helix Log - A beautiful TUI for git history
//
// Architecture:
// - commits.rs: Git commit data structures and loading
// - ui.rs: Ratatui UI components and rendering
// - app.rs: Application state and event handling
// - actions.rs: User actions (navigation, filtering, etc)

pub mod app;
pub mod commits;
pub mod ui;
pub mod actions;

use anyhow::Result;
use std::path::Path;

/// Entry point for `helix log` command
pub fn run(repo_path: Option<&Path>) -> Result<()> {
    let repo_path = repo_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    let mut app = app::App::new(&repo_path)?;
    app.run()?;

    Ok(())
}
