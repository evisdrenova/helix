pub mod actions;
pub mod app;
pub mod ui;

use anyhow::Result;
use std::path::Path;

pub fn run(repo_path: Option<&Path>) -> Result<()> {
    let repo_path = repo_path
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get the current directory."));

    let mut app = app::App::new(&repo_path)?;
    app.run()?;

    Ok(())
}
