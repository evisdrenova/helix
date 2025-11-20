use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use helix::fsmonitor::FSMonitor;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::path::Path;
use std::{io, path::PathBuf};

use super::actions::Action;
use super::ui;

pub enum FileStatus {
    Modified(PathBuf),
    Added(PathBuf),
    Deleted(PathBuf),
    Untracked(PathBuf),
}

pub enum FilterMode {
    All,
}

pub struct StatusResult {
    pub modified: Vec<PathBuf>,
    pub added: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
}

pub struct App {
    pub files: Vec<FileStatus>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub fsmonitor: FSMonitor,
    pub should_quit: bool,
    pub show_untracked: bool,
    pub filter_mode: FilterMode,
}

impl StatusResult {
    pub fn new() -> Self {
        Self {
            modified: Vec::new(),
            added: Vec::new(),
            deleted: Vec::new(),
            untracked: Vec::new(),
        }
    }

    pub fn is_clean(&self) -> bool {
        self.modified.is_empty()
            && self.added.is_empty()
            && self.deleted.is_empty()
            && self.untracked.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.modified.len() + self.added.len() + self.deleted.len() + self.untracked.len()
    }
}

impl App {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let files: Vec<FileStatus> = Vec::new();

        let fsmonitor = FSMonitor::new(repo_path)?;

        Ok(Self {
            files: files,
            selected_index: 0,
            scroll_offset: 0,
            fsmonitor,
            should_quit: true,
            show_untracked: true,
            filter_mode: FilterMode::All,
        })
    }

    // pub fn update_visible_height(&mut self, terminal_height: u16) {
    //     let main_content_height = terminal_height.saturating_sub(4);
    //     let inner_height = main_content_height.saturating_sub(2);
    //     self.visible_height = (inner_height / 4).max(1) as usize;
    // }

    // /// Handle user actions
    // pub fn handle_action(&mut self, action: Action) -> Result<()> {
    //     // Get the list we're navigating
    //     let visible = self.visible_commits();
    //     let visible_count = visible.len();

    //     if visible_count == 0 {
    //         return Ok(()); // No commits to navigate
    //     }

    //     match action {
    //         Action::Quit => {
    //             self.should_quit = true;
    //         }
    //         Action::MoveUp => {
    //             // Find current position in visible list
    //             if let Some(pos) = visible
    //                 .iter()
    //                 .position(|(idx, _)| *idx == self.selected_index)
    //             {
    //                 if pos > 0 {
    //                     self.selected_index = visible[pos - 1].0;
    //                     self.adjust_scroll();
    //                 }
    //             }
    //         }
    //         Action::MoveDown => {
    //             // Find current position in visible list
    //             if let Some(pos) = visible
    //                 .iter()
    //                 .position(|(idx, _)| *idx == self.selected_index)
    //             {
    //                 if pos < visible_count - 1 {
    //                     self.selected_index = visible[pos + 1].0;
    //                     self.adjust_scroll();
    //                 }
    //             } else if !visible.is_empty() {
    //                 // Selection not in visible list, jump to first
    //                 self.selected_index = visible[0].0;
    //                 self.scroll_offset = 0;
    //             }

    //             // Load more commits if we're near the end (only when not filtering)
    //             if self.filtered_indices.is_empty()
    //                 && self.selected_index >= self.commits.len().saturating_sub(10)
    //             {
    //                 self.load_more_commits()?;
    //             }
    //         }
    //         Action::PageUp => {
    //             if let Some(pos) = visible
    //                 .iter()
    //                 .position(|(idx, _)| *idx == self.selected_index)
    //             {
    //                 let new_pos = pos.saturating_sub(10);
    //                 self.selected_index = visible[new_pos].0;
    //                 self.adjust_scroll();
    //             }
    //         }
    //         Action::PageDown => {
    //             if let Some(pos) = visible
    //                 .iter()
    //                 .position(|(idx, _)| *idx == self.selected_index)
    //             {
    //                 let new_pos = (pos + 10).min(visible_count - 1);
    //                 self.selected_index = visible[new_pos].0;
    //                 self.adjust_scroll();
    //             }

    //             // Load more if needed (only when not filtering)
    //             if self.filtered_indices.is_empty()
    //                 && self.selected_index >= self.commits.len().saturating_sub(10)
    //             {
    //                 self.load_more_commits()?;
    //             }
    //         }
    //         Action::CheckoutCommit => {
    //             if let Some(commit) = self.get_selected_commit() {
    //                 let branch_name = format!("checkout-{}", &commit.short_hash);

    //                 match self
    //                     .loader
    //                     .checkout_commit(&commit.hash, Some(&branch_name))
    //                 {
    //                     Ok(_) => {
    //                         self.should_quit = true;
    //                     }
    //                     Err(e) => {
    //                         eprintln!("Failed to checkout commit: {}", e);
    //                     }
    //                 }
    //             }
    //         }
    //         Action::GoToTop => {
    //             if !visible.is_empty() {
    //                 self.selected_index = visible[0].0;
    //                 self.scroll_offset = 0;
    //             }
    //         }
    //         Action::GoToBottom => {
    //             if !visible.is_empty() {
    //                 self.selected_index = visible[visible_count - 1].0;
    //                 self.adjust_scroll();
    //             }
    //         }
    //         Action::EnterSearchMode | Action::ExitSearchMode => {
    //             // handled in event_loop
    //         }
    //     }

    //     Ok(())
    // }

    // fn adjust_scroll(&mut self) {
    //     // Ensure visible_height is at least 1
    //     let visible_height = self.visible_height.max(1);

    //     // If selected is above visible area, scroll up
    //     if self.selected_index < self.scroll_offset {
    //         self.scroll_offset = self.selected_index;
    //     }
    //     // If selected is below visible area, scroll down
    //     else if self.selected_index >= self.scroll_offset + visible_height {
    //         self.scroll_offset = self.selected_index.saturating_sub(visible_height - 1);
    //     }

    //     // Ensure scroll doesn't go past the end
    //     let max_scroll = self.commits.len().saturating_sub(visible_height);
    //     self.scroll_offset = self.scroll_offset.min(max_scroll);
    // }

    // /// Load more commits (lazy loading)
    // fn load_more_files(&mut self) -> Result<()> {
    //     // if self.total_loaded < self.commits.len() + 50 {
    //     //     let new_limit = self.total_loaded + 50;
    //     //     let new_commits = self.loader.load_commits(new_limit)?;

    //     //     if new_commits.len() > self.commits.len() {
    //     //         self.commits = new_commits;
    //     //         self.total_loaded = self.commits.len();
    //     //     }
    //     // }

    //     Ok(())
    // }

    // pub fn run(&mut self) -> Result<()> {
    //     enable_raw_mode()?;
    //     let mut stdout = io::stdout();
    //     execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    //     let backend = CrosstermBackend::new(stdout);
    //     let mut terminal = Terminal::new(backend)?;

    //     let result = self.event_loop(&mut terminal);

    //     disable_raw_mode()?;
    //     execute!(
    //         terminal.backend_mut(),
    //         LeaveAlternateScreen,
    //         DisableMouseCapture
    //     )?;
    //     terminal.show_cursor()?;

    //     result
    // }

    // fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    //     loop {
    //         let terminal_height = terminal.size()?.height;
    //         self.update_visible_height(terminal_height);

    //         terminal.draw(|f| {
    //             ui::draw(f, self);
    //         })?;

    //         if event::poll(std::time::Duration::from_millis(100))? {
    //             if let Event::Key(key) = event::read()? {
    //                 // Handle branch name input mode
    //                 if self.branch_name_mode {
    //                     match key.code {
    //                         KeyCode::Esc => {
    //                             self.branch_name_mode = false;
    //                             self.branch_name_input.clear();
    //                             self.pending_checkout_hash = None;
    //                         }
    //                         KeyCode::Char(c) => {
    //                             self.branch_name_input.push(c);
    //                         }
    //                         KeyCode::Backspace => {
    //                             self.branch_name_input.pop();
    //                         }
    //                         KeyCode::Enter => {
    //                             if !self.branch_name_input.is_empty() {
    //                                 if let Some(ref hash) = self.pending_checkout_hash {
    //                                     match self
    //                                         .loader
    //                                         .checkout_commit(hash, Some(&self.branch_name_input))
    //                                     {
    //                                         Ok(_) => {
    //                                             self.should_quit = true;
    //                                         }
    //                                         Err(e) => {
    //                                             eprintln!("Failed to checkout commit: {}", e);
    //                                             self.branch_name_mode = false;
    //                                             self.branch_name_input.clear();
    //                                             self.pending_checkout_hash = None;
    //                                         }
    //                                     }
    //                                 }
    //                             }
    //                         }
    //                         _ => {}
    //                     }
    //                     continue;
    //                 }

    //                 // Handle search mode input
    //                 if self.search_mode {
    //                     match key.code {
    //                         KeyCode::Esc => {
    //                             self.search_mode = false;
    //                             self.search_query.clear();
    //                             self.filtered_indices.clear();
    //                         }
    //                         KeyCode::Char(c) => {
    //                             self.search_query.push(c);
    //                             self.update_search()
    //                         }
    //                         KeyCode::Backspace => {
    //                             self.search_query.pop();
    //                             self.update_search();
    //                         }
    //                         KeyCode::Enter => {
    //                             self.search_mode = false;
    //                         }
    //                         _ => {}
    //                     }
    //                     continue;
    //                 }

    //                 let action = match key.code {
    //                     KeyCode::Char('q') | KeyCode::Esc => Some(Action::Quit),
    //                     KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    //                         Some(Action::Quit)
    //                     }
    //                     KeyCode::Char('c') => {
    //                         if let Some(commit) = self.get_selected_commit() {
    //                             let hash = commit.hash.clone();
    //                             let short_hash = commit.short_hash.clone();
    //                             self.branch_name_mode = true;
    //                             self.pending_checkout_hash = Some(hash);
    //                             self.branch_name_input = format!("checkout-{}", short_hash);
    //                         }
    //                         continue;
    //                     }
    //                     KeyCode::Char('s') => {
    //                         self.search_mode = true;
    //                         continue;
    //                     }
    //                     KeyCode::Char('/') => {
    //                         self.search_query.clear();
    //                         self.filtered_indices.clear();
    //                         continue;
    //                     }
    //                     KeyCode::Char('v') => {
    //                         self.vim_mode = true;
    //                         continue;
    //                     }
    //                     KeyCode::Char('j') | KeyCode::Down => Some(Action::MoveDown),
    //                     KeyCode::Char('k') | KeyCode::Up => Some(Action::MoveUp),
    //                     KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    //                         Some(Action::PageDown)
    //                     }
    //                     KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
    //                         Some(Action::PageUp)
    //                     }
    //                     KeyCode::Char('g') => Some(Action::GoToTop),
    //                     KeyCode::Char('G') => Some(Action::GoToBottom),
    //                     KeyCode::PageDown => Some(Action::PageDown),
    //                     KeyCode::PageUp => Some(Action::PageUp),
    //                     KeyCode::Home => Some(Action::GoToTop),
    //                     KeyCode::End => Some(Action::GoToBottom),
    //                     _ => None,
    //                 };
    //                 if let Some(action) = action {
    //                     self.handle_action(action)?;
    //                 }
    //             }
    //         }

    //         if self.should_quit {
    //             break;
    //         }
    //     }

    //     Ok(())
    // }
}
