use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame,
};

use super::app::App;
use super::commits::Commit;

pub fn draw(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(f.area());

    draw_header(f, chunks[0], app);
    draw_main_content(f, chunks[1], app);
    draw_footer(f, chunks[2]);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    // Build header content: repo-name | branch → remote ↑↓ | commits | last commit
    let repo_text = format!(" {} ", app.repo_name);

    // Build branch text with remote tracking
    let branch_text = if let Some(ref remote) = app.remote_branch {
        let mut text = format!(" ◉ {} → {} ", app.get_current_branch_name, remote);

        // Add ahead/behind indicators
        if app.ahead > 0 {
            text.push_str(&format!("↑{} ", app.ahead));
        }
        if app.behind > 0 {
            text.push_str(&format!("↓{} ", app.behind));
        }
        text
    } else {
        // No remote tracking
        format!(" ◉ {} ", app.get_current_branch_name)
    };

    let commit_count = format!(" {} commits ", app.commits.len());

    // Get time of most recent commit (first in list)
    let last_commit_text = if let Some(commit) = app.commits.first() {
        format!(" Last: {} ", commit.relative_time())
    } else {
        " No commits ".to_string()
    };

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            repo_text,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            branch_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(commit_count, Style::default().fg(Color::White)),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(last_commit_text, Style::default().fg(Color::DarkGray)),
    ]))
    .block(
        Block::default().borders(Borders::ALL).title_style(
            Style::default()
                .fg(Color::Blue)
                .add_modifier(Modifier::BOLD),
        ),
    );

    f.render_widget(header, area);
}

fn draw_main_content(f: &mut Frame, area: Rect, app: &App) {
    // calculate split ratio
    let timeline_width = (area.width as f32 * app.split_ratio) as u16;
    let details_width = area.width.saturating_sub(timeline_width);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(timeline_width),
            Constraint::Length(details_width),
        ])
        .split(area);

    draw_timeline(f, chunks[0], app);
    draw_details(f, chunks[1], app);
}

fn draw_timeline(f: &mut Frame, area: Rect, app: &App) {
    let inner_height = area.height.saturating_sub(2) as usize; // Subtract border

    // Calculate visible range
    let visible_start = app.scroll_offset;
    let visible_end = (visible_start + inner_height).min(app.commits.len());

    // Create list items - IMPORTANT: track the actual index in the full commit list
    let items: Vec<ListItem> = (visible_start..visible_end)
        .map(|actual_idx| {
            let commit = &app.commits[actual_idx];
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

    let line2 = Line::from(vec![
        Span::raw("   "),
        Span::styled(&commit.author_name, Style::default().fg(author_color)),
        Span::raw(" · "),
        Span::styled(
            format!("+{} ", commit.insertions),
            Style::default().fg(Color::Green),
        ),
        Span::styled(
            format!("-{} ", commit.insertions),
            Style::default().fg(Color::Red),
        ),
    ]);

    let max_len = 45;
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

    let lines = vec![line1, line2, line3, line4];

    // Apply selection style
    let style = if is_selected {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    ListItem::new(Text::from(lines)).style(style)
}

fn draw_details(f: &mut Frame, area: Rect, app: &App) {
    if let Some(commit) = app.get_selected_commit() {
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
            "g/G",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" top/bottom  "),
        Span::styled(
            "h/l",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" adjust split  "),
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
