// src/log/app.rs
//
// Application state and main event loop

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
    pub current_branch: String,

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
}

impl App {
    /// Create a new App instance
    pub fn new(repo_path: &Path) -> Result<Self> {
        let loader = CommitLoader::open(repo_path)?;
        let current_branch = loader.current_branch()?;

        let initial_limit = 50;
        let commits = loader.load_commits(initial_limit)?;
        let total_loaded = commits.len();

        Ok(Self {
            commits,
            selected_index: 0,
            scroll_offset: 0,
            current_branch,
            should_quit: false,
            split_ratio: 0.35, // 35% for timeline, 65% for details
            loader,
            total_loaded,
            initial_limit,
        })
    }

    /// Get the currently selected commit
    pub fn selected_commit(&self) -> Option<&Commit> {
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
            Action::GoToTop => {
                self.selected_index = 0;
                self.scroll_offset = 0;
            }
            Action::GoToBottom => {
                self.selected_index = self.commits.len().saturating_sub(1);
                self.adjust_scroll();
            }
            Action::AdjustSplitLeft => {
                self.split_ratio = (self.split_ratio - 0.05).max(0.2);
            }
            Action::AdjustSplitRight => {
                self.split_ratio = (self.split_ratio + 0.05).min(0.6);
            }
        }

        Ok(())
    }

    /// Adjust scroll offset to keep selected item visible
    fn adjust_scroll(&mut self) {
        let visible_height = 20; // This will be calculated from terminal height in real impl

        if self.selected_index < self.scroll_offset {
            self.scroll_offset = self.selected_index;
        } else if self.selected_index >= self.scroll_offset + visible_height {
            self.scroll_offset = self.selected_index - visible_height + 1;
        }
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

    /// Main event loop
    pub fn run(&mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Main loop
        let result = self.event_loop(&mut terminal);

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    /// Event loop
    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            // Draw UI
            terminal.draw(|f| {
                ui::draw(f, self);
            })?;

            // Handle events
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
                        KeyCode::Char('h') | KeyCode::Left => Some(Action::AdjustSplitLeft),
                        KeyCode::Char('l') | KeyCode::Right => Some(Action::AdjustSplitRight),
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
