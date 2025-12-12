use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use super::app::{App, Focus};

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

    let branch_count = match app.branches.len() {
        0 => format!(" no branches "),
        1 => format!(" {} branch ", app.branches.len()),
        _ => format!(" {} branches ", app.branches.len()),
    };

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

    let upstream_text = match &branch.upstream {
        Some(upstream) => upstream.clone(),
        None => {
            // Check if this is a default/root branch
            if branch.name == "main" || branch.name == "master" {
                "root".to_string() // or "—" (em dash) for cleaner look
            } else {
                "(no upstream)".to_string()
            }
        }
    };

    let upstream_color = if branch.upstream.is_some() {
        Color::Magenta
    } else {
        Color::DarkGray // Dimmed for missing/default
    };

    let line3 = Line::from(vec![
        Span::raw("  "),
        Span::styled(upstream_text, Style::default().fg(upstream_color)),
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
        // Outer border for the whole right-hand panel
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Details ")
            .title_style(Style::default().fg(Color::Blue));

        let inner = block.inner(area);
        f.render_widget(block, area);

        // Split into: summary (top) + commit list (bottom)-
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(7), // summary
                Constraint::Min(0),    // commit list
            ])
            .split(inner);

        draw_branch_summary(f, chunks[0], branch);
        draw_branch_commit_list(f, chunks[1], branch, app);
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

fn draw_branch_summary(f: &mut Frame, area: Rect, branch: &super::app::BranchInfo) {
    let mut lines: Vec<Line> = Vec::new();

    // Title
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

    // Status (current / normal)
    let status_text = if branch.is_current {
        ("Current branch", Color::Green)
    } else {
        ("Local branch", Color::DarkGray)
    };

    lines.push(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "Status:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(status_text.0, Style::default().fg(status_text.1)),
    ]));

    // Commits
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

    // Upstream / remote tracking
    if let Some(ref upstream) = branch.upstream {
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Upstream:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(upstream.clone(), Style::default().fg(Color::Magenta)),
        ]));
    }

    // Last commit age (if known)
    if let Some(ref commit) = branch.last_commit {
        let age = format_relative_time(commit.commit_time);
        lines.push(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Last commit:",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(age, Style::default().fg(Color::Cyan)),
        ]));
    }

    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });

    f.render_widget(paragraph, area);
}

#[derive(Debug)]
struct CommitListEntry {
    short_hash: String,
    summary: String,
    author: String,
    timestamp: u64,
    is_merge: bool,
    is_head: bool,
}

fn draw_branch_commit_list(f: &mut Frame, area: Rect, branch: &super::app::BranchInfo, app: &App) {
    let entries: Vec<CommitListEntry> = app
        .branch_commit_lists
        .get(&branch.name)
        .map(|commits| {
            commits
                .iter()
                .enumerate()
                .map(|(idx, c)| CommitListEntry {
                    short_hash: c.short_hash(),
                    summary: c.summary().to_string(),
                    author: c.author.clone(),
                    timestamp: c.commit_time,
                    is_merge: c.is_merge(),
                    is_head: idx == 0,
                })
                .collect()
        })
        .unwrap_or_default();

    if entries.is_empty() {
        let empty = Paragraph::new("No commits on this branch yet")
            .style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, area);
        return;
    }

    let items: Vec<ListItem> = entries
        .iter()
        .enumerate()
        .map(|(idx, c)| create_commit_item(c, idx == app.selected_commit_index))
        .collect();

    let mut state = ListState::default();
    if app.focus == Focus::CommitList {
        state.select(Some(app.selected_commit_index));
    } else {
        state.select(None);
    }

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::TOP)
                .title(" Commits ")
                .title_style(Style::default().fg(Color::Blue)),
        )
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        );

    f.render_stateful_widget(list, area, &mut state);
}

fn create_commit_item(entry: &CommitListEntry, is_selected: bool) -> ListItem {
    let bullet = if entry.is_head { "●" } else { "○" };

    let bullet_style = if entry.is_head {
        Style::default().fg(Color::Green)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let hash_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let summary_style = if is_selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    };

    let meta_style = Style::default().fg(Color::DarkGray);

    let time_str = format_relative_time(entry.timestamp);

    // Line 1: bullet, short hash, summary
    let line1 = Line::from(vec![
        Span::raw(" "),
        Span::styled(bullet.to_string(), bullet_style),
        Span::raw(" "),
        Span::styled(entry.short_hash.clone(), hash_style),
        Span::raw("  "),
        Span::styled(entry.summary.clone(), summary_style),
    ]);

    // Line 2: author • time • merge badge
    let mut meta_spans = vec![
        Span::raw("   "),
        Span::styled(entry.author.clone(), meta_style),
        Span::raw("  •  "),
        Span::styled(time_str, meta_style),
    ];

    if entry.is_merge {
        meta_spans.push(Span::raw("  •  "));
        meta_spans.push(Span::styled(
            "merge",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let line2 = Line::from(meta_spans);

    ListItem::new(vec![line1, line2])
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
