use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use helix_protocol::{hash, storage::FsObjectStore};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::Path;

use super::ui;
use crate::helix_index::state::get_branch_upstream;
use crate::{
    helix_index::commit::{Commit, CommitStore},
    sandbox_command::RepoContext,
};

#[derive(Debug)]
pub struct BranchInfo {
    pub name: String,
    pub is_current: bool,
    pub last_commit_hash: Option<[u8; 32]>,
    pub last_commit: Option<Commit>,
    pub commit_count: usize,
    pub remote_tracking: Option<String>,
    pub upstream: Option<String>,
}

pub struct App {
    pub branches: Vec<BranchInfo>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub should_quit: bool,
    pub repo_path: std::path::PathBuf,
    pub repo_name: String,
    pub commit_storage: CommitStore,
    pub visible_height: usize,
    pub checkout_mode: bool,
    pub delete_mode: bool,
    pub rename_mode: bool,
    pub new_branch_name: String,
    pub branch_commit_lists: HashMap<String, Vec<Commit>>,
    pub selected_commit_index: usize,
    pub focus: Focus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    BranchList,
    CommitList,
}

impl App {
    pub fn new(start_path: &Path) -> Result<Self> {
        let context = RepoContext::detect(start_path)?;
        let repo_path = &context.repo_root;

        let repo_name = if context.is_sandbox() {
            format!(
                "{} (sandbox: {})",
                repo_path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown"),
                context.sandbox_name().unwrap_or_default()
            )
        } else {
            repo_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string()
        };

        let store = FsObjectStore::new(repo_path);
        let commit_storage = CommitStore::new(repo_path, store)?;
        let current_branch =
            crate::branch_command::get_current_branch(start_path).unwrap_or_default();

        let branch_names = crate::branch_command::get_all_branches(repo_path)?;

        let mut branches = Vec::new();

        for branch_name in branch_names {
            let is_current = branch_name == current_branch;

            // Determine the ref path based on branch type
            let branch_ref_path = if branch_name.starts_with("sandboxes/") {
                let sandbox_name = branch_name.strip_prefix("sandboxes/").unwrap();
                repo_path.join(".helix/refs/sandboxes").join(sandbox_name)
            } else {
                repo_path.join(".helix/refs/heads").join(&branch_name)
            };

            let (last_commit_hash, last_commit, commit_count) =
                if let Ok(hash_hex) = std::fs::read_to_string(&branch_ref_path) {
                    if let Ok(commit_hash) = hash::hex_to_hash(hash_hex.trim()) {
                        if let Ok(commit) = commit_storage.read_commit(&commit_hash) {
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

            let remote_tracking = get_remote_tracking(repo_path, &branch_name);
            let upstream = get_branch_upstream(repo_path, &branch_name);

            branches.push(BranchInfo {
                name: branch_name,
                is_current,
                last_commit_hash,
                last_commit,
                commit_count,
                remote_tracking,
                upstream,
            });
        }

        // Sort: current branch first, then by name
        branches.sort_by(|a, b| match (a.is_current, b.is_current) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        });

        let selected_index = branches.iter().position(|b| b.is_current).unwrap_or(0);

        let mut app = Self {
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
            branch_commit_lists: HashMap::new(),
            selected_commit_index: 0,
            focus: Focus::BranchList,
        };

        // Load commits for the initially selected branch
        app.on_branch_selected()?;

        Ok(app)
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

    /// Called whenever the selected branch changes.
    /// Lazily loads commits for that branch into `branch_commit_lists`.
    fn on_branch_selected(&mut self) -> Result<()> {
        // Figure out which branch is currently selected.
        let branch_name = match self.branches.get(self.selected_index) {
            Some(b) => b.name.clone(),
            None => return Ok(()), // nothing selected
        };

        // Only load if we haven't already cached this branch's commits.
        if !self.branch_commit_lists.contains_key(&branch_name) {
            let commits = self
                .commit_storage
                .load_commits_for_branch(&branch_name, 200)?;
            self.branch_commit_lists.insert(branch_name, commits);
        }

        // Reset commit selection when switching branches.
        self.selected_commit_index = 0;
        Ok(())
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
                crate::branch_command::switch_branch(&self.repo_path, &branch.name)?;
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
            crate::branch_command::delete_branch(
                &self.repo_path,
                &branch_name,
                crate::branch_command::BranchOptions {
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
        crate::branch_command::create_branch(
            &self.repo_path,
            &name,
            crate::branch_command::BranchOptions::default(),
        )?;

        // Reload branches
        *self = Self::new(&self.repo_path)?;
        Ok(())
    }

    pub fn rename_branch(&mut self, new_name: String) -> Result<()> {
        if let Some(branch) = self.selected_branch() {
            let old_name = branch.name.clone();
            crate::branch_command::rename_branch(
                &self.repo_path,
                &old_name,
                &new_name,
                crate::branch_command::BranchOptions::default(),
            )?;

            // Reload branches
            *self = Self::new(&self.repo_path)?;
        }
        Ok(())
    }

    pub fn next_commit(&mut self) {
        if let Some(branch) = self.selected_branch() {
            if let Some(commits) = self.branch_commit_lists.get(&branch.name) {
                if commits.is_empty() {
                    return;
                }
                let max = commits.len().saturating_sub(1);
                if self.selected_commit_index < max {
                    self.selected_commit_index += 1;
                }
            }
        }
    }

    pub fn previous_commit(&mut self) {
        if self.selected_commit_index > 0 {
            self.selected_commit_index -= 1;
        }
    }

    pub fn first_commit(&mut self) {
        self.selected_commit_index = 0;
    }

    pub fn last_commit(&mut self) {
        if let Some(branch) = self.selected_branch() {
            if let Some(commits) = self.branch_commit_lists.get(&branch.name) {
                if !commits.is_empty() {
                    self.selected_commit_index = commits.len() - 1;
                }
            }
        }
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

                        // j / k or Down / Up operate on focused pane
                        KeyCode::Down | KeyCode::Char('j') => match self.focus {
                            Focus::BranchList => {
                                self.next();
                                if let Err(e) = self.on_branch_selected() {
                                    eprintln!("Failed to load commits for branch: {}", e);
                                }
                            }
                            Focus::CommitList => {
                                self.next_commit();
                            }
                        },
                        KeyCode::Up | KeyCode::Char('k') => match self.focus {
                            Focus::BranchList => {
                                self.previous();
                                if let Err(e) = self.on_branch_selected() {
                                    eprintln!("Failed to load commits for branch: {}", e);
                                }
                            }
                            Focus::CommitList => {
                                self.previous_commit();
                            }
                        },

                        // g / G: top/bottom in focused pane
                        KeyCode::Char('g') => match self.focus {
                            Focus::BranchList => {
                                self.go_to_top();
                                if let Err(e) = self.on_branch_selected() {
                                    eprintln!("Failed to load commits for branch: {}", e);
                                }
                            }
                            Focus::CommitList => {
                                self.first_commit();
                            }
                        },
                        KeyCode::Char('G') => match self.focus {
                            Focus::BranchList => {
                                self.go_to_bottom();
                                if let Err(e) = self.on_branch_selected() {
                                    eprintln!("Failed to load commits for branch: {}", e);
                                }
                            }
                            Focus::CommitList => {
                                self.last_commit();
                            }
                        },

                        // 👉 move focus to commit list
                        KeyCode::Right | KeyCode::Char('l') => {
                            if self.focus == Focus::BranchList {
                                if let Some(branch) = self.selected_branch() {
                                    if let Some(commits) =
                                        self.branch_commit_lists.get(&branch.name)
                                    {
                                        if !commits.is_empty() {
                                            self.focus = Focus::CommitList;
                                        }
                                    }
                                }
                            }
                        }

                        // 👈 move focus back to branches
                        KeyCode::Left | KeyCode::Char('h') => {
                            self.focus = Focus::BranchList;
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
                                let branch_name = branch.name.clone();
                                self.rename_mode = true;
                                self.new_branch_name = branch_name;
                            }
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

fn count_commits(storage: &CommitStore, start_hash: &[u8; 32]) -> usize {
    let mut count = 0;
    let mut to_visit = vec![*start_hash];
    let mut seen = HashSet::new();

    while let Some(hash) = to_visit.pop() {
        if !seen.insert(hash) {
            continue;
        }

        count += 1;

        if let Ok(commit) = storage.read_commit(&hash) {
            for parent in &commit.parents {
                to_visit.push(*parent);
            }
        }
    }

    count
}

/// Get the remote tracking branch for a local branch
fn get_remote_tracking(repo_path: &Path, branch_name: &str) -> Option<String> {
    // Try to read from .helix/config or .helix/branch_config
    // Format could be: branch.<branch_name>.remote and branch.<branch_name>.merge
    let config_path = repo_path.join(".helix/config");

    if let Ok(config_content) = std::fs::read_to_string(&config_path) {
        // Parse config file for remote tracking info
        // Example git config format:
        // [branch "main"]
        //     remote = origin
        //     merge = refs/heads/main

        let mut in_branch_section = false;
        let mut remote_name = None;
        let mut merge_ref = None;

        for line in config_content.lines() {
            let trimmed = line.trim();

            // Check if we're entering the right branch section
            if trimmed.starts_with("[branch \"") && trimmed.contains(branch_name) {
                in_branch_section = true;
                continue;
            }

            // Check if we're leaving the section
            if trimmed.starts_with('[') && in_branch_section {
                break;
            }

            if in_branch_section {
                if let Some(remote_value) = trimmed.strip_prefix("remote = ") {
                    remote_name = Some(remote_value.trim().to_string());
                } else if let Some(merge_value) = trimmed.strip_prefix("merge = ") {
                    // Extract branch name from refs/heads/branch_name
                    if let Some(branch_part) = merge_value.strip_prefix("refs/heads/") {
                        merge_ref = Some(branch_part.trim().to_string());
                    }
                }
            }
        }

        // Combine remote and branch name
        if let (Some(remote), Some(branch)) = (remote_name, merge_ref) {
            return Some(format!("{}/{}", remote, branch));
        }
    }

    None
}
