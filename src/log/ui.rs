// src/log/ui.rs
//
// UI rendering with ratatui

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use super::app::App;
use super::commits::Commit;

/// Main draw function
pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(0),    // Main content
            Constraint::Length(1), // Footer
        ])
        .split(f.area());

    // Draw header
    draw_header(f, chunks[0], app);

    // Draw main content (split panes)
    draw_main_content(f, chunks[1], app);

    // Draw footer
    draw_footer(f, chunks[2]);
}

/// Draw the header with branch info
fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let branch_text = format!(" ◉ {}  ", app.current_branch);
    let commit_count = format!("  {} commits loaded ", app.commits.len());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            branch_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(commit_count, Style::default().fg(Color::DarkGray)),
    ]))
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(" helix log ")
            .title_style(
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
    );

    f.render_widget(header, area);
}

/// Draw the main content with split panes
fn draw_main_content(f: &mut Frame, area: Rect, app: &App) {
    // Calculate split ratio
    let timeline_width = (area.width as f32 * app.split_ratio) as u16;
    let details_width = area.width.saturating_sub(timeline_width);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(timeline_width),
            Constraint::Length(details_width),
        ])
        .split(area);

    // Draw timeline (left pane)
    draw_timeline(f, chunks[0], app);

    // Draw details (right pane)
    draw_details(f, chunks[1], app);
}

/// Draw the timeline pane (commit list)
fn draw_timeline(f: &mut Frame, area: Rect, app: &App) {
    let inner_height = area.height.saturating_sub(2) as usize; // Subtract border

    // Calculate visible range
    let visible_start = app.scroll_offset;
    let visible_end = (visible_start + inner_height).min(app.commits.len());

    // Create list items
    let items: Vec<ListItem> = app.commits[visible_start..visible_end]
        .iter()
        .enumerate()
        .map(|(idx, commit)| {
            let actual_idx = visible_start + idx;
            let is_selected = actual_idx == app.selected_index;

            create_timeline_item(commit, is_selected)
        })
        .collect();

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" Timeline ")
            .title_style(Style::default().fg(Color::Blue)),
    );

    f.render_widget(list, area);
}

/// Create a single timeline item
fn create_timeline_item(commit: &Commit, is_selected: bool) -> ListItem {
    let current_user_email = std::env::var("USER").unwrap_or_default();
    let is_current_user = commit.author_email.contains(&current_user_email)
        || commit
            .author_name
            .to_lowercase()
            .contains(&current_user_email.to_lowercase());

    // Time and author
    let time_str = commit.formatted_time();
    let author_indicator = if is_current_user { "●" } else { "○" };
    let author_color = if is_current_user {
        Color::Cyan
    } else {
        Color::Gray
    };

    // First line: time, author, stats
    let line1 = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            author_indicator,
            Style::default()
                .fg(author_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(time_str, Style::default().fg(Color::White)),
    ]);

    // Second line: author name and stats
    let stats = commit.stats_summary();
    let line2 = Line::from(vec![
        Span::raw("   "),
        Span::styled(&commit.author_name, Style::default().fg(author_color)),
        Span::raw(" · "),
        Span::styled(stats, Style::default().fg(Color::DarkGray)),
    ]);

    // Third line: commit summary (truncated)
    let max_len = 45; // Will adjust based on pane width in production
    let summary = if commit.summary.len() > max_len {
        format!("{}...", &commit.summary[..max_len])
    } else {
        commit.summary.clone()
    };

    let summary_style = if is_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let line3 = Line::from(vec![Span::raw("   "), Span::styled(summary, summary_style)]);

    // Empty line for spacing
    let line4 = Line::from(vec![Span::raw("")]);

    let mut lines = vec![line1, line2, line3, line4];

    // Apply selection style
    let style = if is_selected {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    ListItem::new(Text::from(lines)).style(style)
}

/// Draw the details pane
fn draw_details(f: &mut Frame, area: Rect, app: &App) {
    if let Some(commit) = app.selected_commit() {
        let details_text = format_commit_details(commit);

        let paragraph = Paragraph::new(details_text)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Details ")
                    .title_style(Style::default().fg(Color::Blue)),
            )
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    } else {
        let empty = Paragraph::new("No commit selected")
            .block(Block::default().borders(Borders::ALL).title(" Details "))
            .style(Style::default().fg(Color::DarkGray));

        f.render_widget(empty, area);
    }
}

/// Format commit details for display
fn format_commit_details(commit: &Commit) -> Text<'static> {
    let mut lines = vec![];

    // Commit title (summary)
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            commit.summary.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    // Full message (if different from summary)
    let message_body = commit
        .message
        .strip_prefix(&commit.summary)
        .unwrap_or("")
        .trim();

    if !message_body.is_empty() {
        for line in message_body.lines() {
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(line.to_string(), Style::default().fg(Color::White)),
            ]));
        }
        lines.push(Line::from(""));
    }

    // Metadata section
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Commit:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(commit.short_hash.clone(), Style::default().fg(Color::Green)),
    ]));

    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Author:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            commit.author_name.clone(),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Date:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            commit.timestamp.format("%Y-%m-%d %H:%M:%S").to_string(),
            Style::default().fg(Color::White),
        ),
    ]));

    lines.push(Line::from(""));

    // Stats section
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Changes:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    lines.push(Line::from(vec![
        Span::raw("   "),
        Span::styled(
            format!("{} files changed", commit.files_changed),
            Style::default().fg(Color::White),
        ),
    ]));

    if commit.insertions > 0 {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("+{} insertions", commit.insertions),
                Style::default().fg(Color::Green),
            ),
        ]));
    }

    if commit.deletions > 0 {
        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("-{} deletions", commit.deletions),
                Style::default().fg(Color::Red),
            ),
        ]));
    }

    if commit.is_merge {
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled("⚠ Merge commit", Style::default().fg(Color::Yellow)),
        ]));
    }

    Text::from(lines)
}

/// Draw the footer with help text
fn draw_footer(f: &mut Frame, area: Rect) {
    let help_text = Line::from(vec![
        Span::styled(
            " j/k",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" navigate  "),
        Span::styled(
            "h/l",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" adjust split  "),
        Span::styled(
            "g/G",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" top/bottom  "),
        Span::styled(
            "q",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" quit "),
    ]);

    let footer = Paragraph::new(help_text).style(Style::default().bg(Color::DarkGray));

    f.render_widget(footer, area);
}
