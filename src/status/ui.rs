use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use super::app::{App, FileStatus, FilterMode};

pub fn draw(f: &mut Frame, app: &App) {
    if app.show_help {
        draw_help_overlay(f, app);
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(3), // Footer/stats
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_file_list(f, chunks[1], app);
    draw_footer(f, chunks[2], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let repo_text = format!(" 📁 {} ", app.repo_name);

    let filter_text = format!(" Filter: {} ", app.filter_mode.display_name());

    let stats_text = format!(
        " {} files | {} staged ",
        app.files.len(),
        app.staged_files.len()
    );

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            repo_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            filter_text,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(stats_text, Style::default().fg(Color::White)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(" Git Status ")
            .title_style(
                Style::default()
                    .fg(Color::Blue)
                    .add_modifier(Modifier::BOLD),
            ),
    );

    f.render_widget(header, area);
}

fn draw_file_list(f: &mut Frame, area: Rect, app: &App) {
    let visible_files = app.visible_files();

    if visible_files.is_empty() {
        let empty_message = if app.filter_mode == FilterMode::All {
            "✨ Working directory clean!"
        } else {
            "No files match the current filter"
        };

        let empty = Paragraph::new(empty_message)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green))
                    .title(" Files "),
            )
            .style(Style::default().fg(Color::Green))
            .alignment(Alignment::Center);

        f.render_widget(empty, area);
        return;
    }

    let inner_height = area.height.saturating_sub(2) as usize;
    let visible_start = app.scroll_offset;
    let visible_end = (visible_start + inner_height).min(visible_files.len());

    let items: Vec<ListItem> = visible_files[visible_start..visible_end]
        .iter()
        .enumerate()
        .map(|(i, file)| {
            let actual_index = visible_start + i;
            let is_selected = actual_index == app.selected_index;
            let is_staged = app.staged_files.contains(file.path());

            create_file_item(file, is_selected, is_staged)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(" Files ")
            .title_style(Style::default().fg(Color::Blue)),
    );

    f.render_widget(list, area);
}

fn create_file_item(file: &FileStatus, is_selected: bool, is_staged: bool) -> ListItem {
    let status_char = file.status_char();
    let status_color = match file {
        FileStatus::Modified(_) => Color::Yellow,
        FileStatus::Added(_) => Color::Green,
        FileStatus::Deleted(_) => Color::Red,
        FileStatus::Untracked(_) => Color::Cyan,
    };

    let staged_indicator = if is_staged { "✓" } else { " " };
    let selection_indicator = if is_selected { "▶" } else { " " };

    let path_str = file.path().to_string_lossy();

    // Highlight selection
    let path_style = if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let line = Line::from(vec![
        Span::styled(selection_indicator, Style::default().fg(Color::Cyan)),
        Span::raw(" "),
        Span::styled(
            format!("[{}]", status_char),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("[{}]", staged_indicator),
            Style::default().fg(if is_staged {
                Color::Green
            } else {
                Color::DarkGray
            }),
        ),
        Span::raw(" "),
        Span::styled(path_str, path_style),
    ]);

    let style = if is_selected {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    ListItem::new(line).style(style)
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    // Count files by status
    let modified = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Modified(_)))
        .count();
    let added = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Added(_)))
        .count();
    let deleted = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Deleted(_)))
        .count();
    let untracked = app
        .files
        .iter()
        .filter(|f| matches!(f, FileStatus::Untracked(_)))
        .count();

    let stats_line = Line::from(vec![
        Span::styled(
            " M:",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {} ", modified), Style::default().fg(Color::White)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " A:",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {} ", added), Style::default().fg(Color::White)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " D:",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {} ", deleted), Style::default().fg(Color::White)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(
            " ?:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {} ", untracked),
            Style::default().fg(Color::White),
        ),
    ]);

    let help_line = Line::from(vec![
        Span::styled(
            "Space",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" stage  "),
        Span::styled(
            "a",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" all  "),
        Span::styled(
            "A",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" none  "),
        Span::styled(
            "f",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" filter  "),
        Span::styled(
            "r",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" refresh  "),
        Span::styled(
            "?",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" help  "),
        Span::styled(
            "q",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" quit"),
    ]);

    let footer_text = vec![stats_line, help_line];

    let footer = Paragraph::new(footer_text)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .style(Style::default().bg(Color::Black));

    f.render_widget(footer, area);
}

fn draw_help_overlay(f: &mut Frame, app: &App) {
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
        Line::from("  j/k, ↓/↑     Move up/down"),
        Line::from("  Ctrl+d/u     Page down/up"),
        Line::from("  g/G          Go to top/bottom"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Actions",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  Space/Enter  Toggle stage file"),
        Line::from("  a            Stage all files"),
        Line::from("  A            Unstage all files"),
        Line::from("  r            Refresh status"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Filters",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  f            Cycle filter mode"),
        Line::from("  t            Toggle untracked files"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Other",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )]),
        Line::from("  ?            Toggle this help"),
        Line::from("  q/Esc        Quit"),
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
