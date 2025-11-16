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

/// Main application state
pub struct App {
    /// All loaded commits
    pub commits: Vec<Commit>,
    /// Currently selected commit index
    pub selected_index: usize,
    /// Scroll offset for the timeline pane
    pub scroll_offset: usize,
    /// Current branch name
    pub get_current_branch_name: String,
    /// Whether to quit the application
    pub should_quit: bool,
    /// Split ratio (0.0 to 1.0) - how much space the timeline takes
    pub split_ratio: f32,
    /// Commit loader for fetching more commits
    pub loader: CommitLoader,
    /// Total commits loaded
    pub total_loaded: usize,
    /// Maximum commits to load initially
    pub initial_limit: usize,
    /// Repo Name
    pub repo_name: String,
    /// Remote tracking branch (e.g., "origin/main")
    pub remote_branch: Option<String>,
    /// Commits ahead of remote
    pub ahead: usize,
    /// Commits behind remote
    pub behind: usize,
    /// Visible height for timeline (calculated from terminal size)
    pub visible_height: usize,
}

impl App {
    pub fn update_visible_height(&mut self, terminal_height: u16) {
        let main_content_height = terminal_height.saturating_sub(4);
        let inner_height = main_content_height.saturating_sub(2);
        self.visible_height = (inner_height / 4).max(1) as usize;
    }

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
            initial_limit,
            repo_name,
            remote_branch,
            ahead,
            behind,
            visible_height: 20,
        })
    }

    pub fn get_selected_commit(&self) -> Option<&Commit> {
        self.commits.get(self.selected_index)
    }

    /// Handle user actions
    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::MoveUp => {
                if self.selected_index > 0 {
                    self.selected_index -= 1;
                    self.adjust_scroll();
                }
            }
            Action::MoveDown => {
                if self.selected_index < self.commits.len().saturating_sub(1) {
                    self.selected_index += 1;
                    self.adjust_scroll();

                    // Load more commits if we're near the end
                    if self.selected_index >= self.commits.len().saturating_sub(10) {
                        self.load_more_commits()?;
                    }
                }
            }
            Action::PageUp => {
                let page_size = 10;
                self.selected_index = self.selected_index.saturating_sub(page_size);
                self.adjust_scroll();
            }
            Action::PageDown => {
                let page_size = 10;
                self.selected_index =
                    (self.selected_index + page_size).min(self.commits.len().saturating_sub(1));
                self.adjust_scroll();

                if self.selected_index >= self.commits.len().saturating_sub(10) {
                    self.load_more_commits()?;
                }
            }
            Action::CheckoutCommit => {
                //todo: implment this
            }
            Action::GoToTop => {
                self.selected_index = 0;
                self.scroll_offset = 0;
            }
            Action::GoToBottom => {
                self.selected_index = self.commits.len().saturating_sub(1);
                self.adjust_scroll();
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
                    let action = match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
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
