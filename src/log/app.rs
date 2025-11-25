use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::path::Path;

use super::actions::Action;
use super::commits::{Commit, CommitLoader};
use super::ui;

pub struct App {
    pub commits: Vec<Commit>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub get_current_branch_name: String,
    pub should_quit: bool,
    pub split_ratio: f32,
    pub loader: CommitLoader,
    pub total_loaded: usize,
    pub initial_limit: usize,
    pub repo_name: String,
    pub remote_branch: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub visible_height: usize,
    pub search_mode: bool,
    pub vim_mode: bool,
    pub search_query: String,
    pub filtered_indices: Vec<usize>,
    pub branch_name_input: String,
    pub branch_name_mode: bool,
    pub pending_checkout_hash: Option<String>,
}

impl App {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let loader = CommitLoader::open_repo_at_path(repo_path)?;
        let get_current_branch_name = loader.get_current_branch_name()?;

        let initial_limit = 50;
        let commits = loader.load_commits(initial_limit)?;
        let total_loaded = commits.len();
        let repo_name = loader.get_repo_name();

        let (remote_branch, ahead, behind) = loader
            .remote_tracking_info()
            .map(|(branch, ahead, behind)| (Some(branch), ahead, behind))
            .unwrap_or((None, 0, 0));

        Ok(Self {
            commits,
            selected_index: 0,
            scroll_offset: 0,
            get_current_branch_name,
            should_quit: false,
            split_ratio: 0.35, // 35% for timeline, 65% for details
            loader,
            total_loaded,
            initial_limit: initial_limit,
            repo_name,
            remote_branch,
            ahead,
            behind,
            visible_height: 20,
            search_mode: false,
            vim_mode: false,
            search_query: String::new(),
            filtered_indices: Vec::new(),
            branch_name_input: String::new(),
            branch_name_mode: false,
            pending_checkout_hash: None,
        })
    }

    pub fn update_search(&mut self) {
        if self.search_query.is_empty() {
            self.filtered_indices.clear();
            return;
        }

        let query = self.search_query.to_lowercase();
        self.filtered_indices = self
            .commits
            .iter()
            .enumerate()
            .filter(|(_, commit)| {
                commit.summary.to_lowercase().contains(&query)
                    || commit.author_name.to_lowercase().contains(&query)
                    || commit.message.to_lowercase().contains(&query)
                    || commit
                        .file_changes
                        .iter()
                        .any(|f| f.path.to_lowercase().contains(&query))
            })
            .map(|(idx, _)| idx)
            .collect();

        // Reset selection to first match
        if !self.filtered_indices.is_empty() {
            self.selected_index = self.filtered_indices[0];
            self.scroll_offset = 0;
        }
    }

    pub fn visible_commits(&self) -> Vec<(usize, &Commit)> {
        if self.filtered_indices.is_empty() && !self.search_query.is_empty() {
            // Search active but no matches
            Vec::new()
        } else if !self.filtered_indices.is_empty() {
            // Show filtered results
            self.filtered_indices
                .iter()
                .map(|&idx| (idx, &self.commits[idx]))
                .collect()
        } else {
            // Show all commits
            self.commits.iter().enumerate().collect()
        }
    }

    pub fn update_visible_height(&mut self, terminal_height: u16) {
        let main_content_height = terminal_height.saturating_sub(4);
        let inner_height = main_content_height.saturating_sub(2);
        self.visible_height = (inner_height / 4).max(1) as usize;
    }

    pub fn get_selected_commit(&self) -> Option<&Commit> {
        self.commits.get(self.selected_index)
    }

    /// Handle user actions
    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        // Get the list we're navigating
        let visible = self.visible_commits();
        let visible_count = visible.len();

        if visible_count == 0 {
            return Ok(()); // No commits to navigate
        }

        //...

        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::MoveUp => {
                // Find current position in visible list
                if let Some(pos) = visible
                    .iter()
                    .position(|(idx, _)| *idx == self.selected_index)
                {
                    if pos > 0 {
                        self.selected_index = visible[pos - 1].0;
                        self.adjust_scroll();
                    }
                }
            }
            Action::MoveDown => {
                // Find current position in visible list
                if let Some(pos) = visible
                    .iter()
                    .position(|(idx, _)| *idx == self.selected_index)
                {
                    if pos < visible_count - 1 {
                        self.selected_index = visible[pos + 1].0;
                        self.adjust_scroll();
                    }
                } else if !visible.is_empty() {
                    // Selection not in visible list, jump to first
                    self.selected_index = visible[0].0;
                    self.scroll_offset = 0;
                }

                // Load more commits if we're near the end (only when not filtering)
                if self.filtered_indices.is_empty()
                    && self.selected_index >= self.commits.len().saturating_sub(10)
                {
                    self.load_more_commits()?;
                }
            }
            Action::PageUp => {
                if let Some(pos) = visible
                    .iter()
                    .position(|(idx, _)| *idx == self.selected_index)
                {
                    let new_pos = pos.saturating_sub(10);
                    self.selected_index = visible[new_pos].0;
                    self.adjust_scroll();
                }
            }
            Action::PageDown => {
                if let Some(pos) = visible
                    .iter()
                    .position(|(idx, _)| *idx == self.selected_index)
                {
                    let new_pos = (pos + 10).min(visible_count - 1);
                    self.selected_index = visible[new_pos].0;
                    self.adjust_scroll();
                }

                // Load more if needed (only when not filtering)
                if self.filtered_indices.is_empty()
                    && self.selected_index >= self.commits.len().saturating_sub(10)
                {
                    self.load_more_commits()?;
                }
            }
            Action::CheckoutCommit => {
                if let Some(commit) = self.get_selected_commit() {
                    let branch_name = format!("checkout-{}", &commit.short_hash);

                    match self
                        .loader
                        .checkout_commit(&commit.hash, Some(&branch_name))
                    {
                        Ok(_) => {
                            self.should_quit = true;
                        }
                        Err(e) => {
                            eprintln!("Failed to checkout commit: {}", e);
                        }
                    }
                }
            }
            Action::GoToTop => {
                if !visible.is_empty() {
                    self.selected_index = visible[0].0;
                    self.scroll_offset = 0;
                }
            }
            Action::GoToBottom => {
                if !visible.is_empty() {
                    self.selected_index = visible[visible_count - 1].0;
                    self.adjust_scroll();
                }
            }
            Action::EnterSearchMode | Action::ExitSearchMode => {
                // handled in event_loop
            }
        }

        Ok(())
    }

    fn adjust_scroll(&mut self) {
        // Ensure visible_height is at least 1
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
        let max_scroll = self.commits.len().saturating_sub(visible_height);
        self.scroll_offset = self.scroll_offset.min(max_scroll);
    }

    /// Load more commits (lazy loading)
    fn load_more_commits(&mut self) -> Result<()> {
        if self.total_loaded < self.commits.len() + 50 {
            let new_limit = self.total_loaded + 50;
            let new_commits = self.loader.load_commits(new_limit)?;

            if new_commits.len() > self.commits.len() {
                self.commits = new_commits;
                self.total_loaded = self.commits.len();
            }
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
                ui::draw(f, self);
            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // Handle branch name input mode
                    if self.branch_name_mode {
                        match key.code {
                            KeyCode::Esc => {
                                self.branch_name_mode = false;
                                self.branch_name_input.clear();
                                self.pending_checkout_hash = None;
                            }
                            KeyCode::Char(c) => {
                                self.branch_name_input.push(c);
                            }
                            KeyCode::Backspace => {
                                self.branch_name_input.pop();
                            }
                            KeyCode::Enter => {
                                if !self.branch_name_input.is_empty() {
                                    if let Some(ref hash) = self.pending_checkout_hash {
                                        match self
                                            .loader
                                            .checkout_commit(hash, Some(&self.branch_name_input))
                                        {
                                            Ok(_) => {
                                                self.should_quit = true;
                                            }
                                            Err(e) => {
                                                eprintln!("Failed to checkout commit: {}", e);
                                                self.branch_name_mode = false;
                                                self.branch_name_input.clear();
                                                self.pending_checkout_hash = None;
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Handle search mode input
                    if self.search_mode {
                        match key.code {
                            KeyCode::Esc => {
                                self.search_mode = false;
                                self.search_query.clear();
                                self.filtered_indices.clear();
                            }
                            KeyCode::Char(c) => {
                                self.search_query.push(c);
                                self.update_search()
                            }
                            KeyCode::Backspace => {
                                self.search_query.pop();
                                self.update_search();
                            }
                            KeyCode::Enter => {
                                self.search_mode = false;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    let action = match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            Some(Action::Quit)
                        }
                        KeyCode::Char('c') => {
                            if let Some(commit) = self.get_selected_commit() {
                                let hash = commit.hash.clone();
                                let short_hash = commit.short_hash.clone();
                                self.branch_name_mode = true;
                                self.pending_checkout_hash = Some(hash);
                                self.branch_name_input = format!("checkout-{}", short_hash);
                            }
                            continue;
                        }
                        KeyCode::Char('s') => {
                            self.search_mode = true;
                            continue;
                        }
                        KeyCode::Char('/') => {
                            self.search_query.clear();
                            self.filtered_indices.clear();
                            continue;
                        }
                        KeyCode::Char('v') => {
                            self.vim_mode = true;
                            continue;
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
