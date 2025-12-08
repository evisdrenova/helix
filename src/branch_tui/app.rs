use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::collections::HashSet;
use std::io;
use std::path::Path;

use super::ui;
use crate::helix_index::commit::{Commit, CommitStorage};
use crate::helix_index::hash;

pub struct BranchInfo {
    pub name: String,
    pub is_current: bool,
    pub last_commit_hash: Option<[u8; 32]>,
    pub last_commit: Option<Commit>,
    pub commit_count: usize,
}

pub struct App {
    pub branches: Vec<BranchInfo>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub should_quit: bool,
    pub repo_path: std::path::PathBuf,
    pub repo_name: String,
    pub commit_storage: CommitStorage,
    pub visible_height: usize,
    pub checkout_mode: bool,
    pub delete_mode: bool,
    pub rename_mode: bool,
    pub new_branch_name: String,
}

impl App {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let repo_name = repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let commit_storage = CommitStorage::for_repo(repo_path);
        let current_branch = crate::branch::get_current_branch(repo_path).unwrap_or_default();
        let branch_names = crate::branch::get_all_branches(repo_path)?;

        let mut branches = Vec::new();

        for branch_name in branch_names {
            let is_current = branch_name == current_branch;

            // Read branch ref to get commit hash
            let branch_ref_path = repo_path.join(".helix/refs/heads").join(&branch_name);

            let (last_commit_hash, last_commit, commit_count) =
                if let Ok(hash_hex) = std::fs::read_to_string(&branch_ref_path) {
                    if let Ok(commit_hash) = hash::hex_to_hash(hash_hex.trim()) {
                        if let Ok(commit) = commit_storage.read(&commit_hash) {
                            let count = count_commits(&commit_storage, &commit_hash);
                            (Some(commit_hash), Some(commit), count)
                        } else {
                            (Some(commit_hash), None, 0)
                        }
                    } else {
                        (None, None, 0)
                    }
                } else {
                    (None, None, 0)
                };

            branches.push(BranchInfo {
                name: branch_name,
                is_current,
                last_commit_hash,
                last_commit,
                commit_count,
            });
        }

        // Sort: current branch first, then by name
        branches.sort_by(|a, b| match (a.is_current, b.is_current) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        let selected_index = branches.iter().position(|b| b.is_current).unwrap_or(0);

        Ok(Self {
            branches,
            selected_index,
            scroll_offset: 0,
            should_quit: false,
            repo_path: repo_path.to_path_buf(),
            repo_name,
            commit_storage,
            visible_height: 20,
            checkout_mode: false,
            delete_mode: false,
            rename_mode: false,
            new_branch_name: String::new(),
        })
    }

    pub fn update_visible_height(&mut self, terminal_height: u16) {
        let main_content_height = terminal_height.saturating_sub(4);
        let inner_height = main_content_height.saturating_sub(2);
        self.visible_height = (inner_height / 4).max(1) as usize;
    }

    pub fn selected_branch(&self) -> Option<&BranchInfo> {
        self.branches.get(self.selected_index)
    }

    pub fn next(&mut self) {
        if !self.branches.is_empty() {
            self.selected_index = (self.selected_index + 1) % self.branches.len();
            self.adjust_scroll();
        }
    }

    pub fn previous(&mut self) {
        if !self.branches.is_empty() {
            self.selected_index = if self.selected_index == 0 {
                self.branches.len() - 1
            } else {
                self.selected_index - 1
            };
            self.adjust_scroll();
        }
    }

    pub fn go_to_top(&mut self) {
        if !self.branches.is_empty() {
            self.selected_index = 0;
            self.scroll_offset = 0;
        }
    }

    pub fn go_to_bottom(&mut self) {
        if !self.branches.is_empty() {
            self.selected_index = self.branches.len() - 1;
            self.adjust_scroll();
        }
    }

    fn adjust_scroll(&mut self) {
        let visible_height = self.visible_height.max(1);

        // If selected is above visible area, scroll up
        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        }
        // If selected is below visible area, scroll down
        else if self.selected_index >= self.scroll_offset + visible_height {
            self.scroll_offset = self.selected_index.saturating_sub(visible_height - 1);
        }

        // Ensure scroll doesn't go past the end
        let max_scroll = self.branches.len().saturating_sub(visible_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    pub fn checkout_branch(&mut self) -> Result<()> {
        if let Some(branch) = self.selected_branch() {
            if !branch.is_current {
                crate::branch::switch_branch(&self.repo_path, &branch.name)?;
                self.should_quit = true;
            }
        }
        Ok(())
    }

    pub fn delete_branch(&mut self) -> Result<()> {
        if let Some(branch) = self.selected_branch() {
            if branch.is_current {
                // Can't delete current branch
                return Ok(());
            }

            let branch_name = branch.name.clone();
            crate::branch::delete_branch(
                &self.repo_path,
                &branch_name,
                crate::branch::BranchOptions {
                    force: true,
                    ..Default::default()
                },
            )?;

            // Remove from list
            self.branches.retain(|b| b.name != branch_name);

            // Adjust selection
            if self.selected_index >= self.branches.len() && !self.branches.is_empty() {
                self.selected_index = self.branches.len() - 1;
            }
        }
        Ok(())
    }

    pub fn create_branch(&mut self, name: String) -> Result<()> {
        crate::branch::create_branch(
            &self.repo_path,
            &name,
            crate::branch::BranchOptions::default(),
        )?;

        // Reload branches
        *self = Self::new(&self.repo_path)?;
        Ok(())
    }

    pub fn rename_branch(&mut self, new_name: String) -> Result<()> {
        if let Some(branch) = self.selected_branch() {
            let old_name = branch.name.clone();
            crate::branch::rename_branch(
                &self.repo_path,
                &old_name,
                &new_name,
                crate::branch::BranchOptions::default(),
            )?;

            // Reload branches
            *self = Self::new(&self.repo_path)?;
        }
        Ok(())
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
                ui::draw(f, &self);
            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Handle rename mode
                    if self.rename_mode {
                        match key.code {
                            KeyCode::Esc => {
                                self.rename_mode = false;
                                self.new_branch_name.clear();
                            }
                            KeyCode::Char(c) => {
                                self.new_branch_name.push(c);
                            }
                            KeyCode::Backspace => {
                                self.new_branch_name.pop();
                            }
                            KeyCode::Enter => {
                                if !self.new_branch_name.is_empty() {
                                    if let Err(e) = self.rename_branch(self.new_branch_name.clone())
                                    {
                                        eprintln!("Failed to rename branch: {}", e);
                                    }
                                }
                                self.rename_mode = false;
                                self.new_branch_name.clear();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle checkout confirmation mode
                    if self.checkout_mode {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                                if let Err(e) = self.checkout_branch() {
                                    eprintln!("Failed to checkout branch: {}", e);
                                }
                                self.checkout_mode = false;
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                self.checkout_mode = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle delete confirmation mode
                    if self.delete_mode {
                        match key.code {
                            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                                if let Err(e) = self.delete_branch() {
                                    eprintln!("Failed to delete branch: {}", e);
                                }
                                self.delete_mode = false;
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                self.delete_mode = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Normal mode
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            self.should_quit = true;
                        }
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            self.should_quit = true;
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            self.next();
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            self.previous();
                        }
                        KeyCode::Char('g') => {
                            self.go_to_top();
                        }
                        KeyCode::Char('G') => {
                            self.go_to_bottom();
                        }
                        KeyCode::Char('c') => {
                            // Checkout branch
                            if let Some(branch) = self.selected_branch() {
                                if !branch.is_current {
                                    self.checkout_mode = true;
                                }
                            }
                        }
                        KeyCode::Char('d') => {
                            // Delete branch
                            if let Some(branch) = self.selected_branch() {
                                if !branch.is_current {
                                    self.delete_mode = true;
                                }
                            }
                        }
                        KeyCode::Char('r') => {
                            // Rename branch
                            if let Some(branch) = self.selected_branch() {
                                let branch_name = branch.name.clone(); // Clone before dropping the borrow
                                drop(branch); // Explicitly drop the borrow (optional, happens automatically)
                                self.rename_mode = true;
                                self.new_branch_name = branch_name;
                            }
                        }
                        KeyCode::Char('n') => {
                            // New branch
                            self.rename_mode = true;
                            self.new_branch_name = String::from("new-branch");
                        }
                        KeyCode::Enter => {
                            // Quick checkout (no confirmation)
                            if let Err(e) = self.checkout_branch() {
                                eprintln!("Failed to checkout branch: {}", e);
                            }
                        }
                        _ => {}
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

fn count_commits(storage: &CommitStorage, start_hash: &[u8; 32]) -> usize {
    let mut count = 0;
    let mut to_visit = vec![*start_hash];
    let mut seen = HashSet::new();

    while let Some(hash) = to_visit.pop() {
        if !seen.insert(hash) {
            continue;
        }

        count += 1;

        if let Ok(commit) = storage.read(&hash) {
            for parent in &commit.parents {
                to_visit.push(*parent);
            }
        }
    }

    count
}
