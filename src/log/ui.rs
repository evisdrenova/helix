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
    draw_footer(f, chunks[2], app);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let repo_text = format!(" {} ", app.repo_name);

    let branch_text = if let Some(ref remote) = app.remote_branch {
        let mut text = format!(" ◉ {} → {} ", app.get_current_branch_name, remote);

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
    let inner_height = area.height.saturating_sub(2) as usize;

    let visible_commits = app.visible_commits();

    if visible_commits.is_empty() {
        let empty: Vec<ListItem<'_>> = Vec::new();
        let list = List::new(empty).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Timeline ")
                .title_style(Style::default().fg(Color::Blue)),
        );
        f.render_widget(list, area);
        return;
    }

    // Find the position of selected_index in the filtered list
    let selected_pos = visible_commits
        .iter()
        .position(|(idx, _)| *idx == app.selected_index)
        .unwrap_or(0);

    let scroll_offset = if app.search_query.is_empty() {
        let mut offset = app
            .scroll_offset
            .min(visible_commits.len().saturating_sub(1));

        // Adjust offset if selected item is outside the visible window
        if selected_pos < offset {
            // Selected item is above the visible area
            offset = selected_pos;
        } else if selected_pos >= offset + inner_height {
            // Selected item is below the visible area
            offset = selected_pos.saturating_sub(inner_height - 1);
        }

        offset
    } else {
        // Filtering active: center on selected item
        if selected_pos < inner_height / 2 {
            0
        } else {
            selected_pos.saturating_sub(inner_height / 2)
        }
    };

    let visible_start = scroll_offset;
    let visible_end = (visible_start + inner_height).min(visible_commits.len());

    let items: Vec<ListItem> = visible_commits[visible_start..visible_end]
        .iter()
        .map(|(actual_idx, commit)| {
            let is_selected = *actual_idx == app.selected_index;
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

    let time_str = commit.formatted_time();
    let author_indicator = if is_current_user { "●" } else { "○" };
    let author_color = if is_current_user {
        Color::Cyan
    } else {
        Color::Gray
    };

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
            format!("-{} ", commit.deletions),
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
    let line4 = Line::from(vec![Span::raw("")]);
    let lines = vec![line1, line2, line3, line4];

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

    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Changes:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    let max_path_len = commit
        .file_changes
        .iter()
        .map(|f| f.path.len())
        .max()
        .unwrap_or(0);

    for file_change in &commit.file_changes {
        let padding = " ".repeat(max_path_len - file_change.path.len());

        lines.push(Line::from(vec![
            Span::raw("   "),
            Span::styled(
                format!("{}{}", file_change.path, padding),
                Style::default().fg(Color::White),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{:>3}", file_change.insertions),
                Style::default().fg(Color::Green),
            ),
            Span::raw("  "),
            Span::styled(
                format!("-{}", file_change.deletions),
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

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let help_text = if app.branch_name_mode {
        Line::from(vec![
            Span::styled(" Branch name: ", Style::default().fg(Color::Cyan)),
            Span::styled(&app.branch_name_input, Style::default().fg(Color::Yellow)),
            Span::styled("_", Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" to checkout  "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" to cancel"),
        ])
    } else if app.search_mode {
        Line::from(vec![
            Span::styled(" Search: ", Style::default().fg(Color::Cyan)),
            Span::styled(&app.search_query, Style::default().fg(Color::Yellow)),
            Span::styled("_", Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" to cancel"),
        ])
    } else if app.vim_mode {
        Line::from(vec![
            Span::styled(
                " VIM MODE ",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" (not implemented yet)"),
        ])
    } else {
        Line::from(vec![
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
                "c",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" checkout  "),
            Span::styled(
                "s",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" type to search  "),
            Span::styled(
                "q",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" quit "),
        ])
    };

    let footer = Paragraph::new(help_text).style(Style::default().bg(Color::DarkGray));

    f.render_widget(footer, area);
}
