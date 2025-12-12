// // this is the ui for the branch command

use crate::helix_index::hash;
use chrono::{Local, TimeZone};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use super::app::App;

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
    let mut branch_count: String = String::new();

    match app.branches.len() {
        0 => branch_count = format!(" no branches "),
        1 => branch_count = format!(" {} branch ", app.branches.len()),
        _ => branch_count = format!(" {} branches ", app.branches.len()),
    }

    let current_branch_text = if let Some(branch) = app.branches.iter().find(|b| b.is_current) {
        format!(" ● {} ", branch.name)
    } else {
        " No branch ".to_string()
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
            current_branch_text,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
        Span::styled(branch_count, Style::default().fg(Color::White)),
    ]))
    .block(Block::default().borders(Borders::ALL));

    f.render_widget(header, area);
}

fn draw_main_content(f: &mut Frame, area: Rect, app: &App) {
    let timeline_width = (area.width as f32 * 0.35) as u16;
    let details_width = area.width.saturating_sub(timeline_width);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(timeline_width),
            Constraint::Length(details_width),
        ])
        .split(area);

    draw_branch_list(f, chunks[0], app);
    draw_branch_details(f, chunks[1], app);
}

fn draw_branch_list(f: &mut Frame, area: Rect, app: &App) {
    if app.branches.is_empty() {
        let empty_list = List::new(Vec::<ListItem>::new()).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Branches ")
                .title_style(Style::default().fg(Color::Blue)),
        );
        f.render_widget(empty_list, area);
        return;
    }

    let items: Vec<ListItem> = app
        .branches
        .iter()
        .enumerate()
        .map(|(i, branch)| {
            let is_selected = i == app.selected_index;
            create_branch_item(branch, is_selected)
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(app.selected_index));

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Branches ")
                .title_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(list, area, &mut state);
}

fn create_branch_item(branch: &super::app::BranchInfo, is_selected: bool) -> ListItem {
    let indicator = if branch.is_current { "● " } else { "  " };

    let indicator_style = if branch.is_current {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let name_style = if is_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if branch.is_current {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::White)
    };

    let time_str = if let Some(ref commit) = branch.last_commit {
        format_relative_time(commit.commit_time)
    } else {
        "no commits".to_string()
    };

    let line1 = Line::from(vec![
        Span::styled(indicator, indicator_style),
        Span::styled(&branch.name, name_style),
    ]);

    let line2 = Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{} commits", branch.commit_count),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    let remote = match &branch.remote_tracking {
        Some(e) => e.to_string(),
        None => "Detached".to_string(),
    };

    let line3 = Line::from(vec![
        Span::raw("  "),
        Span::styled(remote, Style::default().fg(Color::Cyan)),
    ]);

    let line4 = Line::from(vec![
        Span::raw("  "),
        Span::styled(time_str, Style::default().fg(Color::Cyan)),
    ]);

    let line5 = Line::from(vec![Span::raw("")]);

    let lines = vec![line1, line2, line3, line4, line5];

    let style = if is_selected {
        Style::default().bg(Color::DarkGray)
    } else {
        Style::default()
    };

    ListItem::new(lines).style(style)
}

fn draw_branch_details(f: &mut Frame, area: Rect, app: &App) {
    if let Some(branch) = app.selected_branch() {
        let details_text = format_branch_details(branch);

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
        let empty = Paragraph::new("No branch selected")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Details ")
                    .title_style(Style::default().fg(Color::Blue)),
            )
            .style(Style::default().fg(Color::DarkGray));

        f.render_widget(empty, area);
    }
}

fn format_branch_details(branch: &super::app::BranchInfo) -> ratatui::text::Text<'static> {
    let mut lines = vec![];

    // Branch name
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Title:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            branch.name.clone(),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(""));

    // Remote tracking (if available)
    if let Some(ref remote) = branch.remote_tracking {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Remote:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(remote.clone(), Style::default().fg(Color::Magenta)),
        ]));
        lines.push(Line::from(""));
    }

    // Status
    if branch.is_current {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Status:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled("Current branch", Style::default().fg(Color::Green)),
        ]));
        lines.push(Line::from(""));
    }

    // Commit count
    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Commits:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{}", branch.commit_count),
            Style::default().fg(Color::White),
        ),
    ]));
    lines.push(Line::from(""));

    // Latest commit details
    if let Some(ref commit) = branch.last_commit {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Latest Commit",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));

        // Commit hash - short ID on its own line, full hash below
        let commit_hash_short = short_hash(&commit.commit_hash);
        let commit_hash_full = hash::hash_to_hex(&commit.commit_hash);

        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Short Hash:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(commit_hash_short, Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(""));

        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Full Hash:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(commit_hash_full, Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(""));

        // Author
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Author:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(commit.author.clone(), Style::default().fg(Color::White)),
        ]));
        lines.push(Line::from(""));

        // Date
        let date_str = format_full_time(commit.commit_time);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Last Commit Date:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(date_str, Style::default().fg(Color::White)), // Already owned
        ]));
        lines.push(Line::from(""));

        // Tree
        let tree_hash_short = hash::hash_to_hex(&commit.tree_hash)[..8].to_string();
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Tree:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(tree_hash_short, Style::default().fg(Color::Magenta)),
        ]));
        lines.push(Line::from(""));

        // Message
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Message:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(""));

        for line in commit.message.lines() {
            lines.push(Line::from(vec![
                Span::raw("   "),
                Span::styled(line.to_string(), Style::default().fg(Color::White)),
            ]));
        }

        // Parents (if any)
        if !commit.parents.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "Parents:",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));

            for parent in &commit.parents {
                let parent_hash_short = hash::hash_to_hex(parent)[..8].to_string();

                // let parent_commit_name =
                lines.push(Line::from(vec![
                    Span::raw("   "),
                    Span::styled(parent_hash_short, Style::default().fg(Color::Blue)),
                ]));
            }
        }

        // Merge commit indicator
        if commit.parents.len() > 1 {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "⚠ Merge commit",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }

        // Initial commit indicator
        if commit.parents.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "✨ Initial commit",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    } else {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled("No commits yet", Style::default().fg(Color::DarkGray)),
        ]));
    }

    ratatui::text::Text::from(lines)
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let help_text = if app.rename_mode {
        Line::from(vec![
            Span::styled(" Branch name: ", Style::default().fg(Color::Cyan)),
            Span::styled(&app.new_branch_name, Style::default().fg(Color::Yellow)),
            Span::styled("_", Style::default().fg(Color::Yellow)),
            Span::raw("  "),
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" to confirm  "),
            Span::styled("Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" to cancel"),
        ])
    } else if app.checkout_mode {
        Line::from(vec![
            Span::styled(" Checkout ", Style::default().fg(Color::Yellow)),
            Span::styled(
                app.selected_branch().map(|b| b.name.as_str()).unwrap_or(""),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("?  "),
            Span::styled("y/Enter", Style::default().fg(Color::Green)),
            Span::raw(" yes  "),
            Span::styled("n/Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" no"),
        ])
    } else if app.delete_mode {
        Line::from(vec![
            Span::styled(" Delete ", Style::default().fg(Color::Red)),
            Span::styled(
                app.selected_branch().map(|b| b.name.as_str()).unwrap_or(""),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("?  "),
            Span::styled("y/Enter", Style::default().fg(Color::Green)),
            Span::raw(" yes  "),
            Span::styled("n/Esc", Style::default().fg(Color::DarkGray)),
            Span::raw(" no"),
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
                "d",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" delete  "),
            Span::styled(
                "r",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" rename  "),
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

fn format_relative_time(timestamp: u64) -> String {
    let now = chrono::Utc::now().timestamp() as u64;
    let seconds = now.saturating_sub(timestamp);

    if seconds < 60 {
        format!("{} seconds ago", seconds)
    } else if seconds < 3600 {
        format!("{} minutes ago", seconds / 60)
    } else if seconds < 86400 {
        format!("{} hours ago", seconds / 3600)
    } else if seconds < 2592000 {
        format!("{} days ago", seconds / 86400)
    } else if seconds < 31536000 {
        format!("{} months ago", seconds / 2592000)
    } else {
        format!("{} years ago", seconds / 31536000)
    }
}

fn format_full_time(timestamp: u64) -> String {
    if let Some(dt) = Local.timestamp_opt(timestamp as i64, 0).single() {
        dt.format("%a %b %d %H:%M:%S %Y").to_string()
    } else {
        "Unknown time".to_string()
    }
}

fn short_hash(hash: &[u8; 32]) -> String {
    hash::hash_to_hex(hash)[..8].to_string()
}
