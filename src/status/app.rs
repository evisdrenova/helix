use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use helix::{fsmonitor::FSMonitor, helix_index::api::HelixIndex};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::{collections::HashSet, path::Path, process::Command};
use std::{io, path::PathBuf};

use super::actions::Action;
use super::ui;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    Unstaged,
    Staged,
}

#[derive(Default, Debug)]
pub struct StageStatus {
    pub staged: HashSet<PathBuf>,
    pub modified: HashSet<PathBuf>,
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

#[derive(Debug, Clone, PartialEq)]
pub enum FilterMode {
    All,
    Modified,
    Added,
    Deleted,
    Untracked,
}

impl FilterMode {
    pub fn next(&self) -> Self {
        match self {
            FilterMode::All => FilterMode::Modified,
            FilterMode::Modified => FilterMode::Added,
            FilterMode::Added => FilterMode::Deleted,
            FilterMode::Deleted => FilterMode::Untracked,
            FilterMode::Untracked => FilterMode::All,
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
    pub filter_mode: FilterMode,
    pub visible_height: usize,
    pub repo_path: PathBuf,
    pub repo_name: String,
    pub auto_refresh: bool,
    pub last_refresh: std::time::Instant,
    pub staged_files: HashSet<PathBuf>,
    pub show_help: bool,
    pub current_section: Section,
    pub sections_collapsed: HashSet<Section>,
    pub current_branch: Option<String>,
    pub helix_index: HelixIndex,
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
            HelixIndex::load_or_rebuild(&repo_path).context("Failed to load Helix index")?;

        let mut fsmonitor = FSMonitor::new(&repo_path)?;
        fsmonitor.start_watching_repo()?;

        let mut app = Self {
            files: Vec::new(),
            selected_index: 0,
            scroll_offset: 0,
            fsmonitor,
            should_quit: false,
            show_untracked: true,
            filter_mode: FilterMode::All,
            visible_height: 20,
            repo_path,
            repo_name,
            auto_refresh: true,
            last_refresh: std::time::Instant::now(),
            staged_files: HashSet::new(),
            show_help: false,
            current_section: Section::Unstaged,
            sections_collapsed: HashSet::new(),
            current_branch,
            helix_index,
        };

        app.refresh_status()?;
        Ok(app)
    }

    // todo: look at these in more detail. i don't like that we ahve to call git porcelain here
    fn get_git_status(&self) -> Result<StageStatus> {
        let output = Command::new("git")
            .current_dir(&self.repo_path)
            .args(&["status", "--porcelain"])
            .output()
            .context("Failed to run git status")?;

        let output_str = String::from_utf8_lossy(&output.stdout);

        let mut status = StageStatus::default();

        for line in output_str.lines() {
            if line.len() < 4 {
                continue;
            }

            // Git status format: XY filename
            // X = index status, Y = worktree status
            let index_status = line.chars().nth(0).unwrap();
            let worktree_status = line.chars().nth(1).unwrap();
            let path = PathBuf::from(line[3..].trim());

            // If index status is not ' ' or '?', file is staged
            if index_status != ' ' && index_status != '?' {
                status.staged.insert(path.clone());
            }

            // If worktree status is not ' ', file has modifications
            if worktree_status != ' ' {
                status.modified.insert(path.clone());
            }
        }

        Ok(status)
    }

    fn sync_staging_from_git(&mut self, git_status: &StageStatus) {
        // Clear and rebuild staged files from git's truth index
        self.staged_files.clear();

        for path in &git_status.staged {
            self.staged_files.insert(path.clone());
        }
    }

    pub fn update_visible_height(&mut self, terminal_height: u16) {
        let main_content_height = terminal_height.saturating_sub(4);
        let inner_height = main_content_height.saturating_sub(2);
        self.visible_height = inner_height as usize;
    }

    pub fn refresh_status(&mut self) -> Result<()> {
        self.files.clear();

        // Snapshot dirty working-tree paths once
        let dirty_files = self.fsmonitor.get_dirty_files();

        // 1) If .git/index changed, refresh the helix index (TRACKED/STAGED side)
        if self.fsmonitor.index_changed() {
            if dirty_files.is_empty() {
                // Index changed but no specific files dirty - do full refresh
                self.helix_index.full_refresh()?;
            } else {
                // Incremental update - FAST PATH
                self.helix_index.incremental_refresh(&dirty_files)?;
            }

            self.fsmonitor.clear_index_flag();
        }

        // 2) Apply working tree changes to EntryFlags (MODIFIED/DELETED/UNTRACKED)
        if !dirty_files.is_empty() {
            self.helix_index.apply_worktree_changes(&dirty_files)?;
            // We've consumed the dirty set for this refresh cycle
            self.fsmonitor.clear_dirty();
        }

        // 3) Staged files come directly from the helix index
        self.staged_files = self.helix_index.get_staged();

        // 4) Build UI-level FileStatus from the helix index flags
        let mut seen: HashSet<PathBuf> = HashSet::new();

        // Untracked files (if we’re showing them)
        if self.show_untracked {
            for path in self.helix_index.get_untracked() {
                seen.insert(path.clone());
                self.files.push(FileStatus::Untracked(path));
            }
        }

        // Deleted files
        for path in self.helix_index.get_deleted() {
            if seen.insert(path.clone()) {
                self.files.push(FileStatus::Deleted(path));
            }
        }

        // Modified (unstaged) files
        for path in self.helix_index.get_unstaged() {
            if seen.insert(path.clone()) {
                self.files.push(FileStatus::Modified(path));
            }
        }

        // Sort files for stable display
        self.files.sort_by(|a, b| a.path().cmp(b.path()));

        self.last_refresh = std::time::Instant::now();
        Ok(())
    }

    pub fn visible_files(&self) -> Vec<&FileStatus> {
        let filtered: Vec<&FileStatus> = match self.filter_mode {
            FilterMode::All => self.files.iter().collect(),
            FilterMode::Modified => self
                .files
                .iter()
                .filter(|f| matches!(f, FileStatus::Modified(_)))
                .collect(),
            FilterMode::Added => self
                .files
                .iter()
                .filter(|f| matches!(f, FileStatus::Added(_)))
                .collect(),
            FilterMode::Deleted => self
                .files
                .iter()
                .filter(|f| matches!(f, FileStatus::Deleted(_)))
                .collect(),
            FilterMode::Untracked => self
                .files
                .iter()
                .filter(|f| matches!(f, FileStatus::Untracked(_)))
                .collect(),
        };

        filtered
    }

    /// Get the currently selected file
    pub fn get_selected_file(&self) -> Option<&FileStatus> {
        let visible = self.visible_files();
        if visible.is_empty() {
            return None;
        }
        visible.get(self.selected_index).copied()
    }

    /// Toggle staging for the selected file
    pub fn toggle_stage(&mut self) {
        if let Some(file) = self.get_selected_file() {
            let path = file.path().to_path_buf();
            if self.staged_files.contains(&path) {
                self.staged_files.remove(&path);
            } else {
                self.staged_files.insert(path);
            }
        }
    }

    /// Stage all files
    pub fn stage_all(&mut self) {
        for file in &self.files {
            self.staged_files.insert(file.path().to_path_buf());
        }
    }

    /// Unstage all files
    pub fn unstage_all(&mut self) {
        self.staged_files.clear();
    }

    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        let visible = self.visible_files();
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
                self.toggle_stage();
            }
            Action::StageAll => {
                self.stage_all();
            }
            Action::UnstageAll => {
                self.unstage_all();
            }
            Action::Refresh => {
                self.refresh_status()?;
                // Reset selection if out of bounds
                let visible_count = self.visible_files().len();
                if self.selected_index >= visible_count && visible_count > 0 {
                    self.selected_index = visible_count - 1;
                }
            }
            Action::ToggleFilter => {
                self.filter_mode = self.filter_mode.next();
                self.selected_index = 0;
                self.scroll_offset = 0;
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
        let visible_count = self.visible_files().len();
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

            // Check if .git/index changed (external git add/reset/restore)
            if self.fsmonitor.index_changed() {
                let git_status = self.get_git_status()?;
                self.sync_staging_from_git(&git_status);
                self.fsmonitor.clear_index_flag();
            }

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
                        KeyCode::Char('f') => Some(Action::ToggleFilter),
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

fn get_current_branch(repo_path: &Path) -> Result<String> {
    use std::process::Command;

    let output = Command::new("git")
        .current_dir(repo_path)
        .args(&["branch", "--show-current"])
        .output()?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}
