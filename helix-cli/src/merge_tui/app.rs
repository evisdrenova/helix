use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

use helix_protocol::hash::Hash;
use helix_protocol::message::ObjectType;
use helix_protocol::storage::FsObjectStore;
use ratatui::{backend::CrosstermBackend, Terminal};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use crate::merge_command::{
    analyze_merge, execute_merge, ConflictResolution, MergeAnalysis, MergeConflict, MergeResult,
};

use super::actions::Action;
use super::ui;

/// State for a single conflict being resolved
#[derive(Debug, Clone)]
pub struct ConflictState {
    pub conflict: MergeConflict,
    pub resolution: Option<ConflictResolution>,
    pub target_content: Vec<u8>,
    pub sandbox_content: Vec<u8>,
    pub base_content: Option<Vec<u8>>,
    pub expanded: bool,
}

pub struct App {
    pub repo_path: PathBuf,
    pub target_branch: String,
    pub sandbox_name: String,
    pub base_commit: Hash,
    pub target_commit: Hash,
    pub sandbox_commit: Hash,
    pub analysis: MergeAnalysis,
    pub conflicts: Vec<ConflictState>,
    pub selected_conflict: usize,
    pub scroll_offset: usize,
    pub visible_height: usize,
    pub should_quit: bool,
    pub should_cancel: bool,
    pub show_help: bool,
    pub author: String,
    pub diff_scroll: usize,
    pub diff_max_scroll: usize,
}

impl App {
    pub fn new(
        repo_path: &Path,
        target_branch: &str,
        sandbox_name: &str,
        base_commit: Hash,
        target_commit: Hash,
        sandbox_commit: Hash,
        author: &str,
    ) -> Result<Self> {
        let analysis = analyze_merge(repo_path, &base_commit, &target_commit, &sandbox_commit)?;

        let store = FsObjectStore::new(repo_path);

        // Load content for each conflict
        let conflicts: Vec<ConflictState> = analysis
            .conflicts
            .iter()
            .map(|conflict| {
                let target_content = conflict
                    .target
                    .map(|h| store.read_object(&ObjectType::Blob, &h))
                    .transpose()
                    .ok()
                    .flatten()
                    .unwrap_or_default();

                let sandbox_content = conflict
                    .sandbox
                    .map(|h| store.read_object(&ObjectType::Blob, &h))
                    .transpose()
                    .ok()
                    .flatten()
                    .unwrap_or_default();

                let base_content = conflict
                    .base
                    .map(|h| store.read_object(&ObjectType::Blob, &h))
                    .transpose()
                    .ok()
                    .flatten();

                ConflictState {
                    conflict: conflict.clone(),
                    resolution: None,
                    target_content,
                    sandbox_content,
                    base_content,
                    expanded: false,
                }
            })
            .collect();

        Ok(Self {
            repo_path: repo_path.to_path_buf(),
            target_branch: target_branch.to_string(),
            sandbox_name: sandbox_name.to_string(),
            base_commit,
            target_commit,
            sandbox_commit,
            analysis,
            conflicts,
            selected_conflict: 0,
            scroll_offset: 0,
            visible_height: 20,
            should_quit: false,
            should_cancel: false,
            show_help: false,
            author: author.to_string(),
            diff_scroll: 0,
            diff_max_scroll: 0,
        })
    }

    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }

    pub fn all_resolved(&self) -> bool {
        self.conflicts.iter().all(|c| c.resolution.is_some())
    }

    pub fn resolved_count(&self) -> usize {
        self.conflicts
            .iter()
            .filter(|c| c.resolution.is_some())
            .count()
    }

    pub fn selected_conflict(&self) -> Option<&ConflictState> {
        self.conflicts.get(self.selected_conflict)
    }

    pub fn selected_conflict_mut(&mut self) -> Option<&mut ConflictState> {
        self.conflicts.get_mut(self.selected_conflict)
    }

    pub fn update_visible_height(&mut self, terminal_height: u16) {
        self.visible_height = terminal_height.saturating_sub(10) as usize;
    }

    // ===== Diff scrolling methods =====

    pub fn scroll_diff_up(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_sub(1);
    }

    pub fn scroll_diff_down(&mut self) {
        if self.diff_scroll < self.diff_max_scroll {
            self.diff_scroll += 1;
        }
    }

    pub fn scroll_diff_page_up(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_sub(10);
    }

    pub fn scroll_diff_page_down(&mut self) {
        self.diff_scroll = (self.diff_scroll + 10).min(self.diff_max_scroll);
    }

    pub fn scroll_diff_top(&mut self) {
        self.diff_scroll = 0;
    }

    pub fn scroll_diff_bottom(&mut self) {
        self.diff_scroll = self.diff_max_scroll;
    }

    // ===== Conflict navigation methods =====

    pub fn select_next_conflict(&mut self) {
        if self.selected_conflict < self.conflicts.len().saturating_sub(1) {
            self.selected_conflict += 1;
            self.diff_scroll = 0;
            self.adjust_scroll();
        }
    }

    pub fn select_prev_conflict(&mut self) {
        if self.selected_conflict > 0 {
            self.selected_conflict -= 1;
            self.diff_scroll = 0;
            self.adjust_scroll();
        }
    }

    pub fn select_next_unresolved(&mut self) {
        let start = self.selected_conflict;

        // Search forward from current position
        for i in (start + 1)..self.conflicts.len() {
            if self.conflicts[i].resolution.is_none() {
                self.selected_conflict = i;
                self.diff_scroll = 0;
                self.adjust_scroll();
                return;
            }
        }

        // Wrap around to beginning
        for i in 0..start {
            if self.conflicts[i].resolution.is_none() {
                self.selected_conflict = i;
                self.diff_scroll = 0;
                self.adjust_scroll();
                return;
            }
        }
    }

    pub fn select_prev_unresolved(&mut self) {
        let start = self.selected_conflict;

        // Search backward from current position
        for i in (0..start).rev() {
            if self.conflicts[i].resolution.is_none() {
                self.selected_conflict = i;
                self.diff_scroll = 0;
                self.adjust_scroll();
                return;
            }
        }

        // Wrap around to end
        for i in (start + 1..self.conflicts.len()).rev() {
            if self.conflicts[i].resolution.is_none() {
                self.selected_conflict = i;
                self.diff_scroll = 0;
                self.adjust_scroll();
                return;
            }
        }
    }

    fn adjust_scroll(&mut self) {
        let visible_height = self.visible_height.max(1);

        if self.selected_conflict < self.scroll_offset {
            self.scroll_offset = self.selected_conflict;
        } else if self.selected_conflict >= self.scroll_offset + visible_height {
            self.scroll_offset = self.selected_conflict.saturating_sub(visible_height - 1);
        }
    }

    // ===== Resolution methods =====

    pub fn resolve_current(&mut self, resolution: ConflictResolution) {
        if let Some(conflict_state) = self.conflicts.get_mut(self.selected_conflict) {
            conflict_state.resolution = Some(resolution);
        }

        // Auto-advance to next unresolved conflict
        self.move_to_next_unresolved();
    }

    pub fn resolve_current_with_both(&mut self) {
        if let Some(conflict_state) = self.conflicts.get_mut(self.selected_conflict) {
            // Concatenate target and sandbox content
            let mut merged = conflict_state.target_content.clone();
            if !merged.is_empty() && !merged.ends_with(&[b'\n']) {
                merged.push(b'\n');
            }
            merged.extend_from_slice(&conflict_state.sandbox_content);

            conflict_state.resolution = Some(ConflictResolution::Merged(merged));
        }

        // Auto-advance to next unresolved conflict
        self.move_to_next_unresolved();
    }

    fn move_to_next_unresolved(&mut self) {
        // Find next unresolved conflict after current
        for i in (self.selected_conflict + 1)..self.conflicts.len() {
            if self.conflicts[i].resolution.is_none() {
                self.selected_conflict = i;
                self.diff_scroll = 0;
                self.adjust_scroll();
                return;
            }
        }
        // Wrap around to beginning
        for i in 0..self.selected_conflict {
            if self.conflicts[i].resolution.is_none() {
                self.selected_conflict = i;
                self.diff_scroll = 0;
                self.adjust_scroll();
                return;
            }
        }
    }

    pub fn toggle_expanded(&mut self) {
        if let Some(conflict_state) = self.conflicts.get_mut(self.selected_conflict) {
            conflict_state.expanded = !conflict_state.expanded;
        }
    }

    // ===== Action handling =====

    pub fn handle_action(&mut self, action: Action) -> Result<()> {
        match action {
            Action::Quit => {
                self.should_quit = true;
            }
            Action::Cancel => {
                self.should_cancel = true;
            }

            // Conflict navigation
            Action::NextConflict => {
                self.select_next_conflict();
            }
            Action::PrevConflict => {
                self.select_prev_conflict();
            }
            Action::NextUnresolved => {
                self.select_next_unresolved();
            }
            Action::PrevUnresolved => {
                self.select_prev_unresolved();
            }

            // Diff scrolling
            Action::ScrollDiffUp => {
                self.scroll_diff_up();
            }
            Action::ScrollDiffDown => {
                self.scroll_diff_down();
            }
            Action::ScrollDiffPageUp => {
                self.scroll_diff_page_up();
            }
            Action::ScrollDiffPageDown => {
                self.scroll_diff_page_down();
            }
            Action::ScrollDiffTop => {
                self.scroll_diff_top();
            }
            Action::ScrollDiffBottom => {
                self.scroll_diff_bottom();
            }

            // Resolution actions
            Action::TakeTarget => {
                self.resolve_current(ConflictResolution::TakeTarget);
            }
            Action::TakeSandbox => {
                self.resolve_current(ConflictResolution::TakeSandbox);
            }
            Action::TakeBase => {
                if let Some(conflict_state) = self.selected_conflict() {
                    if conflict_state.base_content.is_some() {
                        self.resolve_current(ConflictResolution::TakeBase);
                    }
                }
            }
            Action::TakeBoth => {
                self.resolve_current_with_both();
            }

            Action::ToggleExpand => {
                self.toggle_expanded();
            }
            Action::Confirm => {
                if self.all_resolved() {
                    self.should_quit = true;
                }
            }
            Action::ToggleHelp => {
                self.show_help = !self.show_help;
            }
        }

        Ok(())
    }

    /// Execute the merge with all resolutions
    pub fn execute(&self) -> Result<MergeResult> {
        let resolutions: HashMap<PathBuf, ConflictResolution> = self
            .conflicts
            .iter()
            .filter_map(|c| c.resolution.clone().map(|r| (c.conflict.path.clone(), r)))
            .collect();

        let message = format!(
            "Merge sandbox '{}' into '{}'",
            self.sandbox_name, self.target_branch
        );

        execute_merge(
            &self.repo_path,
            &self.analysis,
            &resolutions,
            &self.target_commit,
            &self.sandbox_commit,
            &self.author,
            &message,
        )
    }

    pub fn run(&mut self) -> Result<Option<MergeResult>> {
        // If no conflicts, return immediately
        if !self.has_conflicts() {
            return Ok(Some(self.execute()?));
        }

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

    fn event_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<Option<MergeResult>> {
        loop {
            let terminal_height = terminal.size()?.height;
            self.update_visible_height(terminal_height);

            terminal.draw(|f| {
                ui::draw(f, self);
            })?;

            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    // If help is showing, any key closes it
                    if self.show_help {
                        self.show_help = false;
                        continue;
                    }

                    let action = match key.code {
                        KeyCode::Char('q') => Some(Action::Quit),
                        KeyCode::Esc => Some(Action::Cancel),
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            Some(Action::Cancel)
                        }

                        // Conflict navigation (j/k)
                        KeyCode::Char('j') => Some(Action::NextConflict),
                        KeyCode::Char('k') => Some(Action::PrevConflict),
                        KeyCode::Char('n') => Some(Action::NextUnresolved),
                        KeyCode::Char('p') => Some(Action::PrevUnresolved),

                        // Diff scrolling (arrow keys)
                        KeyCode::Up => Some(Action::ScrollDiffUp),
                        KeyCode::Down => Some(Action::ScrollDiffDown),
                        KeyCode::PageUp => Some(Action::ScrollDiffPageUp),
                        KeyCode::PageDown => Some(Action::ScrollDiffPageDown),
                        KeyCode::Home => Some(Action::ScrollDiffTop),
                        KeyCode::End => Some(Action::ScrollDiffBottom),

                        // Resolution actions
                        KeyCode::Char('t') | KeyCode::Char('1') => Some(Action::TakeTarget),
                        KeyCode::Char('s') | KeyCode::Char('2') => Some(Action::TakeSandbox),
                        KeyCode::Char('b') | KeyCode::Char('3') => Some(Action::TakeBase),
                        KeyCode::Char('a') => Some(Action::TakeBoth),

                        // Other actions
                        KeyCode::Tab | KeyCode::Char('e') => Some(Action::ToggleExpand),
                        KeyCode::Enter => Some(Action::Confirm),
                        KeyCode::Char('?') => Some(Action::ToggleHelp),

                        _ => None,
                    };

                    if let Some(action) = action {
                        self.handle_action(action)?;
                    }
                }
            }

            if self.should_cancel {
                return Ok(None);
            }

            if self.should_quit && self.all_resolved() {
                return Ok(Some(self.execute()?));
            }
        }
    }
}
