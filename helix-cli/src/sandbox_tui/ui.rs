// sandbox_tui/ui.rs

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::sandbox_command::{SandboxChange, SandboxChangeKind};

use super::app::{App, SandboxInfo, Section};

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
    draw_main_content(f, chunks[1], app);
    draw_help_bar(f, chunks[2]);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let sandbox_count = app.sandboxes.len();
    let total_changes: usize = app.sandboxes.iter().map(|s| s.changes.len()).sum();

    let clean_count = app
        .sandboxes
        .iter()
        .filter(|s| s.changes.is_empty())
        .count();
    let dirty_count = sandbox_count - clean_count;

    let header_line_1 = Line::from(vec![
        Span::styled("Repo: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            &app.repo_name,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled("Sandboxes: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", sandbox_count),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let header_line_2 = Line::from(vec![
        Span::styled("Clean: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", clean_count),
            Style::default().fg(Color::Green),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled("Dirty: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", dirty_count),
            Style::default().fg(if dirty_count > 0 {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled("Total changes: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!("{}", total_changes),
            Style::default().fg(if total_changes > 0 {
                Color::Yellow
            } else {
                Color::Green
            }),
        ),
    ]);

    let header = Paragraph::new(vec![header_line_1, header_line_2]).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(" Helix Sandboxes "),
    );

    f.render_widget(header, area);
}

fn draw_main_content(f: &mut Frame, area: Rect, app: &App) {
    if app.sandboxes.is_empty() {
        draw_empty_state(f, area);
        return;
    }

    // Split into sandboxes list and details panel
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(40), // Sandbox list
            Constraint::Percentage(60), // Details + changes
        ])
        .split(area);

    draw_sandbox_list(f, chunks[0], app);
    draw_sandbox_details(f, chunks[1], app);
}

fn draw_sandbox_list(f: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.current_section == Section::Sandboxes;
    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let items: Vec<ListItem> = app
        .sandboxes
        .iter()
        .enumerate()
        .map(|(idx, sandbox)| {
            create_sandbox_item(sandbox, idx == app.selected_sandbox_index, is_focused)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(" Sandboxes "),
    );

    f.render_widget(list, area);
}

fn create_sandbox_item(sandbox: &SandboxInfo, is_selected: bool, is_focused: bool) -> ListItem {
    let indicator = if is_selected && is_focused {
        "▶ "
    } else {
        "  "
    };

    let status_color = if sandbox.changes.is_empty() {
        Color::Green
    } else {
        Color::Yellow
    };

    let status_icon = if sandbox.changes.is_empty() {
        "✓"
    } else {
        "●"
    };

    let age = format_age(sandbox.manifest.created_at);
    let change_summary = sandbox.change_summary();

    let name_style = if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let line = Line::from(vec![
        Span::raw(indicator),
        Span::styled(status_icon, Style::default().fg(status_color)),
        Span::raw(" "),
        Span::styled(&sandbox.manifest.name, name_style),
        Span::raw(" "),
        Span::styled(format!("({})", age), Style::default().fg(Color::DarkGray)),
        Span::raw(" "),
        Span::styled(change_summary, Style::default().fg(status_color)),
    ]);

    ListItem::new(line)
}

fn draw_sandbox_details(f: &mut Frame, area: Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(8), // Info panel
            Constraint::Min(0),    // Changes list
        ])
        .split(area);

    draw_info_panel(f, chunks[0], app);
    draw_changes_list(f, chunks[1], app);
}

fn draw_info_panel(f: &mut Frame, area: Rect, app: &App) {
    let content = if let Some(sandbox) = app.get_selected_sandbox() {
        let base_short = &sandbox.manifest.base_commit[..8.min(sandbox.manifest.base_commit.len())];
        let created = format_timestamp(sandbox.manifest.created_at);
        let branch = sandbox.manifest.branch.as_deref().unwrap_or("(none)");
        let description = sandbox
            .manifest
            .description
            .as_deref()
            .unwrap_or("No description");

        vec![
            Line::from(vec![
                Span::styled("Name:        ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &sandbox.manifest.name,
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Base commit: ", Style::default().fg(Color::DarkGray)),
                Span::styled(base_short, Style::default().fg(Color::Yellow)),
            ]),
            Line::from(vec![
                Span::styled("Branch:      ", Style::default().fg(Color::DarkGray)),
                Span::styled(branch, Style::default().fg(Color::Green)),
            ]),
            Line::from(vec![
                Span::styled("Created:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(created, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("Description: ", Style::default().fg(Color::DarkGray)),
                Span::styled(description, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("Workdir:     ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    sandbox.workdir.to_string_lossy(),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ]
    } else {
        vec![Line::from(Span::styled(
            "No sandbox selected",
            Style::default().fg(Color::DarkGray),
        ))]
    };

    let info = Paragraph::new(content).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(" Details "),
    );

    f.render_widget(info, area);
}

fn draw_changes_list(f: &mut Frame, area: Rect, app: &App) {
    let is_focused = app.current_section == Section::Changes;
    let border_color = if is_focused {
        Color::Cyan
    } else {
        Color::DarkGray
    };

    let items: Vec<ListItem> = if let Some(sandbox) = app.get_selected_sandbox() {
        if sandbox.changes.is_empty() {
            vec![ListItem::new(Line::from(Span::styled(
                "  No changes - sandbox is clean",
                Style::default().fg(Color::Green),
            )))]
        } else {
            sandbox
                .changes
                .iter()
                .enumerate()
                .map(|(idx, change)| {
                    create_change_item(change, idx == app.selected_change_index, is_focused)
                })
                .collect()
        }
    } else {
        vec![ListItem::new(Line::from(Span::styled(
            "  Select a sandbox to view changes",
            Style::default().fg(Color::DarkGray),
        )))]
    };

    let change_count = app
        .get_selected_sandbox()
        .map(|s| s.changes.len())
        .unwrap_or(0);

    let title = format!(" Changes ({}) ", change_count);

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color))
            .title(title),
    );

    f.render_widget(list, area);
}

fn create_change_item(change: &SandboxChange, is_selected: bool, is_focused: bool) -> ListItem {
    let indicator = if is_selected && is_focused {
        "▶ "
    } else {
        "  "
    };

    let (status_char, status_color) = match change.kind {
        SandboxChangeKind::Added => ('A', Color::Green),
        SandboxChangeKind::Modified => ('M', Color::Yellow),
        SandboxChangeKind::Deleted => ('D', Color::Red),
    };

    let path_style = if is_selected {
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
        Span::styled(change.path.to_string_lossy(), path_style),
    ]);

    ListItem::new(line)
}

fn draw_empty_state(f: &mut Frame, area: Rect) {
    let message = vec![
        Line::from(""),
        Line::from(Span::styled(
            "No sandboxes found",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Create one with:",
            Style::default().fg(Color::DarkGray),
        )),
        Line::from(Span::styled(
            "  helix sandbox create <name>",
            Style::default().fg(Color::Cyan),
        )),
    ];

    let empty = Paragraph::new(message)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .alignment(Alignment::Center);

    f.render_widget(empty, area);
}

fn draw_help_bar(f: &mut Frame, area: Rect) {
    let help_text = vec![Line::from(vec![
        Span::styled("↑/↓", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" move • "),
        Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" switch section • "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" refresh • "),
        Span::styled("?", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" help • "),
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" quit"),
    ])];

    let help = Paragraph::new(help_text).block(Block::default().borders(Borders::ALL));

    f.render_widget(help, area);
}

fn draw_help_overlay(f: &mut Frame) {
    let area = centered_rect(60, 60, f.area());

    let help_text = vec![
        Line::from(Span::styled(
            "Sandbox Manager Help",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Navigation",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  j/k, ↓/↑      Move up/down"),
        Line::from("  Tab           Switch between sandboxes and changes"),
        Line::from("  Ctrl+d/u      Page down/up"),
        Line::from("  g/G           Go to top/bottom"),
        Line::from(""),
        Line::from(Span::styled(
            "Actions",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  r             Refresh sandbox list"),
        Line::from(""),
        Line::from(Span::styled(
            "Other",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  ?             Toggle this help"),
        Line::from("  q             Quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Press ? to close",
            Style::default().fg(Color::DarkGray),
        )),
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

fn format_age(timestamp: u64) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let age_secs = now.saturating_sub(timestamp);

    if age_secs < 60 {
        "just now".to_string()
    } else if age_secs < 3600 {
        format!("{}m ago", age_secs / 60)
    } else if age_secs < 86400 {
        format!("{}h ago", age_secs / 3600)
    } else {
        format!("{}d ago", age_secs / 86400)
    }
}

fn format_timestamp(timestamp: u64) -> String {
    use std::time::{Duration, UNIX_EPOCH};

    let datetime = UNIX_EPOCH + Duration::from_secs(timestamp);

    // Simple formatting without chrono
    let age = format_age(timestamp);
    format!("{} ({})", timestamp, age)
}
