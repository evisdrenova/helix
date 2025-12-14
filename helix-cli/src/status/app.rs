use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use helix_cli::helix_index::{api::HelixIndexData, EntryFlags};
use helix_cli::{fsmonitor::FSMonitor, ignore::IgnoreRules};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use super::actions::Action;
use super::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Unstaged,
    Staged,
    Untracked,
}

#[derive(Debug, Clone, PartialEq)]
pub enum FileStatus {
    Modified(PathBuf),
    Added(PathBuf),
    Deleted(PathBuf),
    Untracked(PathBuf),
}

impl FileStatus {
    pub fn path(&self) -> &Path {
        match self {
            FileStatus::Modified(p) => p,
            FileStatus::Added(p) => p,
            FileStatus::Deleted(p) => p,
            FileStatus::Untracked(p) => p,
        }
    }

    pub fn status_char(&self) -> char {
        match self {
            FileStatus::Modified(_) => 'M',
            FileStatus::Added(_) => 'A',
            FileStatus::Deleted(_) => 'D',
            FileStatus::Untracked(_) => '?',
        }
    }
}

pub struct App {
    pub files: Vec<FileStatus>,
    pub selected_index: usize,
    pub scroll_offset: usize,
    pub fsmonitor: FSMonitor,
    pub should_quit: bool,
    pub show_untracked: bool,
    pub visible_height: usize,
    pub repo_path: PathBuf,
    pub repo_name: String,
    pub auto_refresh: bool,
    pub last_refresh: std::time::Instant,
    pub staged_files: HashSet<PathBuf>,
    pub tracked_files: HashSet<PathBuf>,
    pub show_help: bool,
    pub current_section: Section,
    pub sections_collapsed: HashSet<Section>,
    pub current_branch: Option<String>,
    pub helix_index: HelixIndexData,
    pub ignore_rules: IgnoreRules,
}

impl App {
    pub fn new(repo_path: &Path) -> Result<Self> {
        let repo_path = repo_path.canonicalize()?;
        let repo_name = repo_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("repository")
            .to_string();
        let current_branch = get_current_branch(&repo_path).ok();

        let helix_index =
            HelixIndexData::load_or_rebuild(&repo_path).context("Failed to load Helix index")?;

        let mut fsmonitor = FSMonitor::new(&repo_path)?;
        fsmonitor.start_watching_repo()?;

        let ignore_rules = IgnoreRules::load(&repo_path);

        let mut app = Self {
            files: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            fsmonitor,
            should_quit: false,
            show_untracked: true,
            visible_height: 20,
            repo_path,
            repo_name,
            auto_refresh: true,
            last_refresh: std::time::Instant::now(),
            staged_files: HashSet::new(),
            tracked_files: HashSet::new(),
            show_help: false,
            current_section: Section::Unstaged,
            sections_collapsed: HashSet::new(),
            current_branch,
            helix_index,
            ignore_rules,
        };

        app.refresh_status()?;
        Ok(app)
    }

    pub fn update_visible_height(&mut self, terminal_height: u16) {
        let main_content_height = terminal_height.saturating_sub(4);
        let inner_height = main_content_height.saturating_sub(2);
        self.visible_height = inner_height as usize;
    }

    pub fn refresh_status(&mut self) -> Result<()> {
        self.files.clear();
        self.tracked_files.clear();
        self.staged_files.clear();

        // If index changed on disk, reload it
        if self.fsmonitor.index_changed() {
            self.helix_index = HelixIndexData::load_or_rebuild(&self.repo_path)?;
            self.fsmonitor.clear_index_flag();
        }

        // Build tracked & staged sets from helix index entries
        let entries = self.helix_index.entries();

        for entry in entries {
            let path = entry.path.clone();
            let flags = entry.flags;

            if flags.contains(EntryFlags::TRACKED) {
                self.tracked_files.insert(path.clone());
            }

            if flags.contains(EntryFlags::STAGED) {
                self.staged_files.insert(path.clone());
            }
        }

        // Build FileStatus from flags (FileStatus is a simple wrapper over the flags)
        let mut seen = std::collections::HashSet::new();

        // tracked entries with changes (staged and/or modified/deleted)
        for entry in entries {
            let path = entry.path.clone();
            let flags = entry.flags;

            if !flags.contains(EntryFlags::TRACKED) {
                continue;
            }

            // Clean file → no status entry
            if !flags.intersects(EntryFlags::STAGED | EntryFlags::MODIFIED | EntryFlags::DELETED) {
                continue;
            }

            // Partially staged case:
            // TRACKED | STAGED | MODIFIED
            // We represent it as "Modified" in FileStatus and let the UI
            // also show it in the STAGED section via self.staged_files.

            if flags.contains(EntryFlags::MODIFIED) {
                if seen.insert(path.clone()) {
                    self.files.push(FileStatus::Modified(path));
                }
            } else if flags.contains(EntryFlags::DELETED) {
                if seen.insert(path.clone()) {
                    self.files.push(FileStatus::Deleted(path));
                }
            } else if flags.contains(EntryFlags::STAGED) {
                // Staged, no extra working-tree changes -> treat as "Added"/"Staged change"
                if seen.insert(path.clone()) {
                    self.files.push(FileStatus::Added(path));
                }
            }
        }

        // Untracked files (not in helix index)
        if self.show_untracked {
            let untracked_paths = self.scan_for_untracked_files()?;
            for path in untracked_paths {
                if seen.insert(path.clone()) {
                    self.files.push(FileStatus::Untracked(path));
                }
            }
        }

        //  Sort for stable display
        self.files.sort_by(|a, b| a.path().cmp(b.path()));

        Ok(())
    }

    /// Scan working tree for untracked files
    /// This catches files that existed before FSMonitor started
    fn scan_for_untracked_files(&mut self) -> Result<Vec<PathBuf>> {
        let mut untracked = Vec::new();

        // No filter_entry that closes over &self
        for entry in WalkDir::new(&self.repo_path)
            .follow_links(false)
            .into_iter()
        {
            let entry = entry?;

            // Now the borrow of &self is only for this call, and ends right after
            if !self.should_process_entry(&entry) {
                continue;
            }

            if !entry.path().is_file() {
                continue;
            }

            let rel_path = entry.path().strip_prefix(&self.repo_path)?;

            // Short immutable borrow for helix_index
            let tracked = self.helix_index.is_tracked(rel_path);
            if tracked {
                // Now we can mutably borrow self.tracked_files safely
                self.tracked_files.insert(rel_path.to_path_buf());
                continue;
            }

            if self.ignore_rules.should_ignore(rel_path) {
                continue;
            }

            untracked.push(rel_path.to_path_buf());
        }

        Ok(untracked)
    }

    fn should_process_entry(&self, entry: &walkdir::DirEntry) -> bool {
        let name = entry.file_name().to_string_lossy();

        // Skip .git and .helix
        if name == ".git" || name == ".helix" {
            return false;
        }

        // For directories, check if ignored
        if entry.path().is_dir() {
            let rel_path = match entry.path().strip_prefix(&self.repo_path) {
                Ok(p) => p,
                Err(_) => return false,
            };

            if self.ignore_rules.should_ignore(rel_path) {
                return false;
            }
        }

        true
    }

    /// Get the currently selected file
    pub fn get_selected_file(&self) -> Option<&FileStatus> {
        if self.files.is_empty() {
            return None;
        }
        self.files.get(self.selected_index)
    }

    /// Toggle staging for the selected file
    pub fn toggle_stage(&mut self) -> Result<()> {
        if let Some(file) = self.get_selected_file() {
            let path = file.path().to_path_buf();

            if self.staged_files.contains(&path) {
                // Unstage: remove STAGED flag
                self.helix_index.unstage_file(&path)?;
                self.staged_files.remove(&path);
            } else {
                // Stage: add STAGED flag
                self.helix_index.stage_file(&path)?;
                self.staged_files.insert(path);
            }

            // Persist changes to .helix/helix.idx
            self.helix_index.persist()?;
        }

        Ok(())
    }

    /// Stage all files
    pub fn stage_all(&mut self) -> Result<()> {
        for file in &self.files {
            let path = file.path().to_path_buf();
            self.helix_index.stage_file(&path)?;
            self.staged_files.insert(path);
        }

        // Persist changes to .helix/helix.idx
        self.helix_index.persist()?;
        Ok(())
    }

    /// Unstage all files
    pub fn unstage_all(&mut self) -> Result<()> {
        for path in self.staged_files.clone() {
            self.helix_index.unstage_file(&path)?;
        }

        self.staged_files.clear();

        // Persist changes to .helix/helix.idx
        self.helix_index.persist()?;
        Ok(())
    }

    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        let visible = self.files.iter();
        let visible_count = visible.len();

        if visible_count == 0
            && !matches!(
                action,
                Action::Quit | Action::Refresh | Action::ToggleHelp | Action::SwitchSection
            )
        {
            return Ok(());
        }

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
                if self.selected_index < visible_count.saturating_sub(1) {
                    self.selected_index += 1;
                    self.adjust_scroll();
                }
            }
            Action::PageUp => {
                self.selected_index = self.selected_index.saturating_sub(10);
                self.adjust_scroll();
            }
            Action::PageDown => {
                self.selected_index =
                    (self.selected_index + 10).min(visible_count.saturating_sub(1));
                self.adjust_scroll();
            }
            Action::GoToTop => {
                self.selected_index = 0;
                self.scroll_offset = 0;
            }
            Action::GoToBottom => {
                self.selected_index = visible_count.saturating_sub(1);
                self.adjust_scroll();
            }
            Action::ToggleStage => {
                self.toggle_stage()?;
            }
            Action::StageAll => {
                self.stage_all()?;
            }
            Action::UnstageAll => {
                self.unstage_all()?;
            }
            Action::Refresh => {
                self.refresh_status()?;
                // Reset selection if out of bounds
                let visible_count = self.files.len();
                if self.selected_index >= visible_count && visible_count > 0 {
                    self.selected_index = visible_count - 1;
                }
            }
            Action::ToggleUntracked => {
                self.show_untracked = !self.show_untracked;
                self.refresh_status()?;
            }
            Action::ToggleHelp => {
                self.show_help = !self.show_help;
            }
            Action::SwitchSection => {
                // Toggle between Unstaged and Staged sections
                self.current_section = match self.current_section {
                    Section::Unstaged => Section::Staged,
                    Section::Staged => Section::Unstaged,
                    Section::Untracked => Section::Staged,
                };
                // Reset selection when switching sections
                self.selected_index = 0;
                self.scroll_offset = 0;
            }
            Action::CollapseSection => {
                // Collapse the current section
                self.sections_collapsed.insert(self.current_section);
            }
            Action::ExpandSection => {
                // Expand the current section
                self.sections_collapsed.remove(&self.current_section);
            }
        }

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
        let visible_count = self.files.len();
        let max_scroll = visible_count.saturating_sub(visible_height);
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

            // Auto-refresh every 2 seconds if enabled
            if self.auto_refresh && self.last_refresh.elapsed().as_secs() >= 2 {
                self.refresh_status()?;
            }

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
                        KeyCode::Char(' ') | KeyCode::Enter => Some(Action::ToggleStage),
                        KeyCode::Char('a') => Some(Action::StageAll),
                        KeyCode::Char('A') => Some(Action::UnstageAll),
                        KeyCode::Char('r') => Some(Action::Refresh),
                        KeyCode::Char('t') => Some(Action::ToggleUntracked),
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

/// Get current branch name from .helix/HEAD
fn get_current_branch(repo_path: &Path) -> Result<String> {
    let head_path = repo_path.join(".helix").join("HEAD");

    if !head_path.exists() {
        return Ok("(no branch)".to_string());
    }

    let content = fs::read_to_string(&head_path)?;
    let content = content.trim();

    if content.starts_with("ref:") {
        let ref_path = content.strip_prefix("ref:").unwrap().trim();

        // Extract branch name from refs/heads/main
        if let Some(branch) = ref_path.strip_prefix("refs/heads/") {
            Ok(branch.to_string())
        } else {
            Ok("(unknown)".to_string())
        }
    } else {
        Ok("(detached HEAD)".to_string())
    }
}
