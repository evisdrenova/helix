/*
The UI for the status command
h - collapses a section
l - expands a section
*/

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::status::app::Section;

use super::app::{App, FileStatus, FilterMode};

pub fn draw(f: &mut Frame, app: &App) {
    if app.show_help {
        draw_help_overlay(f);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(3), // Help bar
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_file_sections(f, chunks[1], app);
    draw_help_bar(f, chunks[2]);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let modified_count = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Modified(_)))
        .count();
    let added_count = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Added(_)))
        .count();
    let deleted_count = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Deleted(_)))
        .count();
    let untracked_count = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Untracked(_)))
        .count();

    let repo_text = format!("Repo: {} ", app.repo_name);
    let branch_text = format!(
        "Branch: {} ",
        app.current_branch.as_deref().unwrap_or("main")
    );

    let stats_line_1 = Line::from(vec![
        Span::raw(repo_text),
        Span::styled("│ ", Style::default().fg(Color::DarkGray)),
        Span::raw(branch_text),
    ]);

    let stats_line_2 = Line::from(vec![
        Span::styled(
            "M:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", modified_count),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            " A:",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", added_count),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            " D:",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", deleted_count),
            Style::default().fg(Color::White),
        ),
        Span::styled(
            " ?:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", untracked_count),
            Style::default().fg(Color::White),
        ),
        Span::raw("  "),
        Span::styled("(m)", Style::default().fg(Color::Yellow)),
        Span::raw("od "),
        Span::styled("(a)", Style::default().fg(Color::Green)),
        Span::raw("dd "),
        Span::styled("(d)", Style::default().fg(Color::Red)),
        Span::raw("el "),
        Span::styled("(u)", Style::default().fg(Color::Cyan)),
        Span::raw("ntracked "),
        Span::styled("(c)", Style::default().fg(Color::Magenta)),
        Span::raw("onflicts "),
        Span::styled("(.)", Style::default().fg(Color::White)),
        Span::raw("All"),
    ]);

    let header = Paragraph::new(vec![stats_line_1, stats_line_2]).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue)),
    );

    f.render_widget(header, area);
}

fn draw_file_sections(f: &mut Frame, area: Rect, app: &App) {
    let visible_files = app.visible_files();

    if visible_files.is_empty() {
        draw_empty_state(f, area, app);
        return;
    }

    let mut unstaged_items = Vec::new();
    let mut staged_items = Vec::new();
    let mut untracked_items = Vec::new();

    for (idx, file) in visible_files.iter().enumerate() {
        let is_selected = idx == app.selected_index;
        let is_staged = app.staged_files.contains(file.path());
        let is_tracked = app.tracked_files.contains(file.path());
        let is_untracked = matches!(file, FileStatus::Untracked(_));
        let is_modified = matches!(file, FileStatus::Modified(_));
        let is_deleted = matches!(file, FileStatus::Deleted(_));

        let item = create_file_item(
            file,
            is_selected,
            is_staged,
            app.current_section == Section::Unstaged && is_selected,
        );

        // only in “UNTRACKED”
        if is_untracked {
            untracked_items.push(item.clone());
            continue;
        }

        //  file has something in the index differing from HEAD
        if is_staged {
            staged_items.push(item.clone());
        }

        // Unstaged changes: working tree differs from index
        if (is_modified || is_deleted) && is_tracked {
            unstaged_items.push(item.clone());
        }
    }

    // Calculate section sizes
    let unstaged_visible = !app.sections_collapsed.contains(&Section::Unstaged);
    let staged_visible = !app.sections_collapsed.contains(&Section::Staged);
    let untracked_visible = !app.sections_collapsed.contains(&Section::Untracked);

    let unstaged_size = if unstaged_visible {
        (unstaged_items.len() + 1).min(20) as u16
    } else {
        1
    };

    let staged_size = if staged_visible {
        (staged_items.len() + 1).min(20) as u16
    } else {
        1
    };

    let untracked_size = if untracked_visible {
        (untracked_items.len() + 1).min(20) as u16
    } else {
        1
    };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(unstaged_size + 2),
            Constraint::Length(staged_size + 2),
            Constraint::Length(untracked_size + 2),
            Constraint::Min(0),
        ])
        .split(area);

    draw_section(
        f,
        sections[0],
        "UNSTAGED (focus)",
        &unstaged_items,
        unstaged_visible,
        app.current_section == Section::Unstaged,
    );

    draw_section(
        f,
        sections[1],
        "STAGED",
        &staged_items,
        staged_visible,
        app.current_section == Section::Staged,
    );
    draw_section(
        f,
        sections[1],
        "UNTRACKED",
        &untracked_items,
        untracked_visible,
        app.current_section == Section::Untracked,
    );
}

fn draw_section(
    f: &mut Frame,
    area: Rect,
    title: &str,
    items: &[ListItem],
    expanded: bool,
    is_focused: bool,
) {
    let expand_symbol = if expanded { "▼" } else { "▶" };
    let full_title = format!(" {} {} ", expand_symbol, title);

    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    if !expanded {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(full_title);
        f.render_widget(block, area);
        return;
    }

    let list = List::new(items.to_vec()).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(full_title),
    );

    f.render_widget(list, area);
}

fn create_file_item(
    file: &FileStatus,
    is_selected: bool,
    is_staged: bool,
    highlight: bool,
) -> ListItem {
    let status_char = file.status_char();
    let status_color = match file {
        FileStatus::Modified(_) => Color::Yellow,
        FileStatus::Added(_) => Color::Green,
        FileStatus::Deleted(_) => Color::Red,
        FileStatus::Untracked(_) => Color::Cyan,
    };

    let indicator = if is_selected { "▶ " } else { "  " };
    let staged_marker = if is_staged { "" } else { "" };

    let path_str = file.path().to_string_lossy();

    let path_style = if highlight {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
            .bg(Color::DarkGray)
    } else if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let line = Line::from(vec![
        Span::raw(indicator),
        Span::styled(
            format!("[{}]", status_char),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("[{}] ", staged_marker),
            Style::default().fg(if is_staged {
                Color::Green
            } else {
                Color::DarkGray
            }),
        ),
        Span::styled(path_str, path_style),
    ]);

    ListItem::new(line)
}

fn draw_empty_state(f: &mut Frame, area: Rect, app: &App) {
    let message = if app.filter_mode == FilterMode::All {
        "Working directory empty"
    } else {
        "No files match the current filter"
    };

    let empty = Paragraph::new(message)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        )
        .style(Style::default().fg(Color::Green))
        .alignment(Alignment::Center);

    f.render_widget(empty, area);
}

fn draw_help_bar(f: &mut Frame, area: Rect) {
    let help_text = vec![
        Line::from(vec![
            Span::styled("Help: ", Style::default().fg(Color::Cyan)),
            Span::raw("↑/↓ move • "),
            Span::styled("Space", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" toggle stage/unstage • "),
            Span::styled("A", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" stage visible • "),
            Span::styled("U", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" unstage visible •"),
        ]),
        Line::from(vec![
            Span::raw("       "),
            Span::styled("/", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" search • "),
            Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" switch section • "),
            Span::styled("h/l", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" collapse/expand folders • "),
            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" open file • "),
            Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" quit"),
        ]),
    ];

    let help = Paragraph::new(help_text).block(Block::default().borders(Borders::ALL));

    f.render_widget(help, area);
}

fn draw_help_overlay(f: &mut Frame) {
    let area = centered_rect(60, 70, f.area());

    let help_text = vec![
        Line::from(vec![Span::styled(
            "Git Status Help",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Navigation",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  j/k, ↓/↑      Move up/down"),
        Line::from("  Tab           Switch between sections"),
        Line::from("  h/l           Collapse/expand sections"),
        Line::from("  Ctrl+d/u      Page down/up"),
        Line::from("  g/G           Go to top/bottom"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Actions",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  Space/Enter   Toggle stage file"),
        Line::from("  A             Stage all visible files"),
        Line::from("  U             Unstage all visible files"),
        Line::from("  r             Refresh status"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Filters & Search",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  /             Search files"),
        Line::from("  f             Cycle filter mode"),
        Line::from("  Esc           Clear search/filter"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Other",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  ?             Toggle this help"),
        Line::from("  q             Quit"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Press ? to close",
            Style::default().fg(Color::DarkGray),
        )]),
    ];

    let help = Paragraph::new(help_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan))
                .title(" Help ")
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .style(Style::default().bg(Color::Black))
        .wrap(Wrap { trim: false });

    f.render_widget(help, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
