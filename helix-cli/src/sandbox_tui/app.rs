// sandbox_tui/app.rs

use anyhow::{bail, Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use git2::StatusOptions;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::path::{Path, PathBuf};
use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};
use std::{fs, io};

use crate::sandbox_command::{
    get_sandbox_changes, Sandbox, SandboxChange, SandboxChangeKind, SandboxManifest,
};

use super::actions::Action;
use super::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Sandboxes,
    Changes,
}

#[derive(Debug, Clone)]
pub struct SandboxInfo {
    pub manifest: SandboxManifest,
    pub root: PathBuf,
    pub workdir: PathBuf,
    pub changes: Vec<SandboxChange>,
}

impl SandboxInfo {
    pub fn change_summary(&self) -> String {
        if self.changes.is_empty() {
            "clean".to_string()
        } else {
            let added = self
                .changes
                .iter()
                .filter(|c| c.kind == SandboxChangeKind::Added)
                .count();
            let modified = self
                .changes
                .iter()
                .filter(|c| c.kind == SandboxChangeKind::Modified)
                .count();
            let deleted = self
                .changes
                .iter()
                .filter(|c| c.kind == SandboxChangeKind::Deleted)
                .count();

            let mut parts = Vec::new();
            if added > 0 {
                parts.push(format!("+{}", added));
            }
            if modified > 0 {
                parts.push(format!("~{}", modified));
            }
            if deleted > 0 {
                parts.push(format!("-{}", deleted));
            }

            parts.join(" ")
        }
    }
}

pub struct App {
    pub sandboxes: Vec<SandboxInfo>,
    pub selected_sandbox_index: usize,
    pub selected_change_index: usize,
    pub scroll_offset: usize,
    pub should_quit: bool,
    pub visible_height: usize,
    pub repo_path: PathBuf,
    pub repo_name: String,
    pub show_help: bool,
    pub current_section: Section,
    pub sections_collapsed: HashSet<Section>,
    pub last_refresh: std::time::Instant,
}

impl App {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let repo_path = repo_path.canonicalize()?;
        let repo_name = repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repository")
            .to_string();

        let mut app = Self {
            sandboxes: Vec::new(),
            selected_sandbox_index: 0,
            selected_change_index: 0,
            scroll_offset: 0,
            should_quit: false,
            visible_height: 20,
            repo_path,
            repo_name,
            show_help: false,
            current_section: Section::Sandboxes,
            sections_collapsed: HashSet::new(),
            last_refresh: std::time::Instant::now(),
        };

        app.refresh_sandboxes()?;
        Ok(app)
    }

    pub fn update_visible_height(&mut self, terminal_height: u16) {
        let main_content_height = terminal_height.saturating_sub(4);
        let inner_height = main_content_height.saturating_sub(2);
        self.visible_height = inner_height as usize;
    }

    pub fn refresh_sandboxes(&mut self) -> Result<()> {
        self.sandboxes.clear();

        let sandboxes = list_sandboxes_silent(&self.repo_path)?;

        for sandbox in sandboxes {
            let changes =
                get_sandbox_changes(&self.repo_path, &sandbox.manifest.name).unwrap_or_default();

            self.sandboxes.push(SandboxInfo {
                manifest: sandbox.manifest,
                root: sandbox.root,
                workdir: sandbox.workdir,
                changes,
            });
        }

        // Sort by creation time (newest first)
        self.sandboxes
            .sort_by(|a, b| b.manifest.created_at.cmp(&a.manifest.created_at));

        // Reset selection if out of bounds
        if self.selected_sandbox_index >= self.sandboxes.len() && !self.sandboxes.is_empty() {
            self.selected_sandbox_index = self.sandboxes.len() - 1;
        }

        self.last_refresh = std::time::Instant::now();

        Ok(())
    }

    pub fn get_selected_sandbox(&self) -> Option<&SandboxInfo> {
        self.sandboxes.get(self.selected_sandbox_index)
    }

    pub fn get_selected_change(&self) -> Option<&SandboxChange> {
        self.get_selected_sandbox()
            .and_then(|s| s.changes.get(self.selected_change_index))
    }
    pub fn list_sandboxes(repo_path: &Path) -> Result<Vec<Sandbox>> {
        let sandboxes_dir = repo_path.join(".helix").join("sandboxes");

        if !sandboxes_dir.exists() {
            println!("No sandboxes found.");
            println!("Create one with: helix sandbox create <name>");
            return Ok(vec![]);
        }

        let mut sandboxes = Vec::new();

        for entry in fs::read_dir(&sandboxes_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                let manifest_path = path.join("manifest.toml");
                if manifest_path.exists() {
                    if let Ok(manifest) = SandboxManifest::load(&path) {
                        sandboxes.push(Sandbox {
                            manifest,
                            root: path.clone(),
                            workdir: path.join("workdir"),
                        });
                    }
                }
            }
        }

        if sandboxes.is_empty() {
            println!("No sandboxes found.");
            println!("Create one with: helix sandbox create <name>");
            return Ok(vec![]);
        }

        // Sort by creation time (newest first)
        sandboxes.sort_by(|a, b| b.manifest.created_at.cmp(&a.manifest.created_at));

        println!("Sandboxes:\n");

        for sandbox in &sandboxes {
            let age = format_age(sandbox.manifest.created_at);
            let base_short = &sandbox.manifest.base_commit[..8];

            // Quick status check
            let status = get_sandbox_status_summary(repo_path, &sandbox.manifest.name);

            println!(
                "  {}  (base: {}, {}, {})",
                sandbox.manifest.name, base_short, age, status
            );
        }

        println!();
        Ok(sandboxes)
    }

    pub fn sandbox_status(
        repo_path: &Path,
        name: &str,
        options: StatusOptions,
    ) -> Result<Vec<SandboxChange>> {
        let sandbox_root = repo_path.join(".helix").join("sandboxes").join(name);

        if !sandbox_root.exists() {
            bail!("Sandbox '{}' does not exist", name);
        }

        let manifest = SandboxManifest::load(&sandbox_root)?;

        println!(
            "Sandbox '{}' (base: {})\n",
            name,
            &manifest.base_commit[..8]
        );

        let changes = get_sandbox_changes(repo_path, name)?;

        if changes.is_empty() {
            println!("No changes in sandbox '{}'", name);
            println!("\nUse existing helix commands in the sandbox workdir:");
            println!("  cd {}", sandbox_root.join("workdir").display());
            println!("  helix status   # view status");
            println!("  helix add .    # stage changes");
            println!("  helix commit   # commit changes");
        } else {
            println!("Changes in sandbox '{}':\n", name);
            for change in &changes {
                println!("  {} {}", change.status_char(), change.path.display());
            }
            println!("\n{} file(s) changed", changes.len());
        }

        Ok(changes)
    }

    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::MoveUp => {
                match self.current_section {
                    Section::Sandboxes => {
                        if self.selected_sandbox_index > 0 {
                            self.selected_sandbox_index -= 1;
                            self.selected_change_index = 0; // Reset change selection
                        }
                    }
                    Section::Changes => {
                        if self.selected_change_index > 0 {
                            self.selected_change_index -= 1;
                        }
                    }
                }
                self.adjust_scroll();
            }
            Action::MoveDown => {
                match self.current_section {
                    Section::Sandboxes => {
                        if self.selected_sandbox_index < self.sandboxes.len().saturating_sub(1) {
                            self.selected_sandbox_index += 1;
                            self.selected_change_index = 0; // Reset change selection
                        }
                    }
                    Section::Changes => {
                        if let Some(sandbox) = self.get_selected_sandbox() {
                            if self.selected_change_index < sandbox.changes.len().saturating_sub(1)
                            {
                                self.selected_change_index += 1;
                            }
                        }
                    }
                }
                self.adjust_scroll();
            }
            Action::PageUp => {
                match self.current_section {
                    Section::Sandboxes => {
                        self.selected_sandbox_index =
                            self.selected_sandbox_index.saturating_sub(10);
                    }
                    Section::Changes => {
                        self.selected_change_index = self.selected_change_index.saturating_sub(10);
                    }
                }
                self.adjust_scroll();
            }
            Action::PageDown => {
                match self.current_section {
                    Section::Sandboxes => {
                        self.selected_sandbox_index = (self.selected_sandbox_index + 10)
                            .min(self.sandboxes.len().saturating_sub(1));
                    }
                    Section::Changes => {
                        if let Some(sandbox) = self.get_selected_sandbox() {
                            self.selected_change_index = (self.selected_change_index + 10)
                                .min(sandbox.changes.len().saturating_sub(1));
                        }
                    }
                }
                self.adjust_scroll();
            }
            Action::GoToTop => {
                match self.current_section {
                    Section::Sandboxes => self.selected_sandbox_index = 0,
                    Section::Changes => self.selected_change_index = 0,
                }
                self.scroll_offset = 0;
            }
            Action::GoToBottom => {
                match self.current_section {
                    Section::Sandboxes => {
                        self.selected_sandbox_index = self.sandboxes.len().saturating_sub(1);
                    }
                    Section::Changes => {
                        if let Some(sandbox) = self.get_selected_sandbox() {
                            self.selected_change_index = sandbox.changes.len().saturating_sub(1);
                        }
                    }
                }
                self.adjust_scroll();
            }
            Action::Refresh => {
                self.refresh_sandboxes()?;
            }
            Action::ToggleHelp => {
                self.show_help = !self.show_help;
            }
            Action::SwitchSection => {
                self.current_section = match self.current_section {
                    Section::Sandboxes => Section::Changes,
                    Section::Changes => Section::Sandboxes,
                };
                self.scroll_offset = 0;
            }
            Action::CollapseSection => {
                self.sections_collapsed.insert(self.current_section);
            }
            Action::ExpandSection => {
                self.sections_collapsed.remove(&self.current_section);
            }
            // These actions don't apply to sandbox TUI
            Action::ToggleStage
            | Action::StageAll
            | Action::UnstageAll
            | Action::ToggleUntracked => {}
        }

        Ok(())
    }

    fn adjust_scroll(&mut self) {
        let visible_height = self.visible_height.max(1);

        let selected = match self.current_section {
            Section::Sandboxes => self.selected_sandbox_index,
            Section::Changes => self.selected_change_index,
        };

        if selected < self.scroll_offset {
            self.scroll_offset = selected;
        } else if selected >= self.scroll_offset + visible_height {
            self.scroll_offset = selected.saturating_sub(visible_height - 1);
        }

        let count = match self.current_section {
            Section::Sandboxes => self.sandboxes.len(),
            Section::Changes => self
                .get_selected_sandbox()
                .map(|s| s.changes.len())
                .unwrap_or(0),
        };

        let max_scroll = count.saturating_sub(visible_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    pub fn run(&mut self) -> Result<()> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        let result = self.event_loop(&mut terminal);

        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            let terminal_height = terminal.size()?.height;
            self.update_visible_height(terminal_height);

            terminal.draw(|f| {
                ui::draw(f, self);
            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    let action = match key.code {
                        KeyCode::Char('q') => Some(Action::Quit),
                        KeyCode::Esc => Some(Action::Quit),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            Some(Action::Quit)
                        }
                        KeyCode::Char('j') | KeyCode::Down => Some(Action::MoveDown),
                        KeyCode::Char('k') | KeyCode::Up => Some(Action::MoveUp),
                        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            Some(Action::PageDown)
                        }
                        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            Some(Action::PageUp)
                        }
                        KeyCode::Char('g') => Some(Action::GoToTop),
                        KeyCode::Char('G') => Some(Action::GoToBottom),
                        KeyCode::PageDown => Some(Action::PageDown),
                        KeyCode::PageUp => Some(Action::PageUp),
                        KeyCode::Home => Some(Action::GoToTop),
                        KeyCode::End => Some(Action::GoToBottom),
                        KeyCode::Char('r') => Some(Action::Refresh),
                        KeyCode::Char('?') => Some(Action::ToggleHelp),
                        KeyCode::Tab => Some(Action::SwitchSection),
                        KeyCode::Char('h') => Some(Action::CollapseSection),
                        KeyCode::Char('l') => Some(Action::ExpandSection),
                        _ => None,
                    };

                    if let Some(action) = action {
                        self.handle_action(action)?;
                    }
                }
            }

            if self.should_quit {
                break;
            }
        }

        Ok(())
    }
}

/// List sandboxes without printing to stdout (for TUI)
fn list_sandboxes_silent(repo_path: &Path) -> Result<Vec<Sandbox>> {
    use std::fs;

    let sandboxes_dir = repo_path.join(".helix").join("sandboxes");

    if !sandboxes_dir.exists() {
        return Ok(vec![]);
    }

    let mut sandboxes = Vec::new();

    for entry in fs::read_dir(&sandboxes_dir)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            let manifest_path = path.join("manifest.toml");
            if manifest_path.exists() {
                if let Ok(manifest) = SandboxManifest::load(&path) {
                    sandboxes.push(Sandbox {
                        manifest,
                        root: path.clone(),
                        workdir: path.join("workdir"),
                    });
                }
            }
        }
    }

    Ok(sandboxes)
}

fn format_age(timestamp: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let age_secs = now.saturating_sub(timestamp);

    if age_secs < 60 {
        "just now".to_string()
    } else if age_secs < 3600 {
        format!("{} min ago", age_secs / 60)
    } else if age_secs < 86400 {
        format!("{} hours ago", age_secs / 3600)
    } else {
        format!("{} days ago", age_secs / 86400)
    }
}

fn get_sandbox_status_summary(repo_path: &Path, name: &str) -> String {
    match get_sandbox_changes(repo_path, name) {
        Ok(changes) => {
            if changes.is_empty() {
                "clean".to_string()
            } else {
                format!("{} changes", changes.len())
            }
        }
        Err(_) => "unknown".to_string(),
    }
}
