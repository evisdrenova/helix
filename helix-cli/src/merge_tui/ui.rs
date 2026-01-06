use merge_command::ConflictType;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{
        Block, Borders, Clear, List, ListItem, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState,
    },
    Frame,
};

use crate::merge_command;

use super::app::{App, ConflictState};

pub fn draw(f: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Header
            Constraint::Min(10),   // Main content
            Constraint::Length(3), // Help bar
        ])
        .split(f.area());

    draw_header(f, app, chunks[0]);
    draw_main(f, app, chunks[1]);
    draw_help_bar(f, app, chunks[2]);

    if app.show_help {
        draw_help_popup(f, f.area());
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let resolved = app.resolved_count();
    let total = app.conflicts.len();

    let status = if app.all_resolved() {
        Span::styled(" ✓ All resolved ", Style::default().fg(Color::Green))
    } else {
        Span::styled(
            format!(" {} / {} resolved ", resolved, total),
            Style::default().fg(Color::Yellow),
        )
    };

    let title = Line::from(vec![
        Span::styled("Merge: ", Style::default().fg(Color::Cyan)),
        Span::styled(&app.sandbox_name, Style::default().fg(Color::Yellow)),
        Span::raw(" → "),
        Span::styled(&app.target_branch, Style::default().fg(Color::Green)),
        Span::raw(" │ "),
        status,
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(title);

    f.render_widget(block, area);
}

fn draw_main(f: &mut Frame, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(area);

    draw_conflict_list(f, app, chunks[0]);
    draw_conflict_detail(f, app, chunks[1]);
}

fn draw_conflict_list(f: &mut Frame, app: &mut App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;

    let items: Vec<ListItem> = app
        .conflicts
        .iter()
        .enumerate()
        .map(|(i, conflict_state)| {
            let is_selected = i == app.selected_conflict;

            let status_icon = match &conflict_state.resolution {
                Some(_) => Span::styled("✓ ", Style::default().fg(Color::Green)),
                None => Span::styled("● ", Style::default().fg(Color::Red)),
            };

            let conflict_type = match conflict_state.conflict.conflict_type {
                ConflictType::BothModified => "mod/mod",
                ConflictType::ModifyDelete => "mod/del",
                ConflictType::BothAdded => "add/add",
            };

            let path = conflict_state.conflict.path.display().to_string();
            let path_short = if path.len() > 20 {
                format!("...{}", &path[path.len() - 17..])
            } else {
                path
            };

            let style = if is_selected {
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let line = Line::from(vec![
                status_icon,
                Span::styled(path_short, style),
                Span::styled(
                    format!(" ({})", conflict_type),
                    Style::default().fg(Color::DarkGray),
                ),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Blue))
                .title(" Conflicts "),
        )
        .highlight_style(Style::default().bg(Color::DarkGray));

    f.render_widget(list, area);

    // Scrollbar for conflict list
    if app.conflicts.len() > inner_height {
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));

        let mut scrollbar_state = ScrollbarState::default()
            .content_length(app.conflicts.len())
            .position(app.selected_conflict);

        f.render_stateful_widget(
            scrollbar,
            area.inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

fn draw_conflict_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(conflict_state) = app.selected_conflict() else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(" No conflicts ");
        f.render_widget(block, area);
        return;
    };

    // Clone the data we need to avoid borrow conflicts
    let conflict_state_clone = conflict_state.clone();

    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Resolution status
            Constraint::Min(5),    // Content diff
            Constraint::Length(5), // Actions
        ])
        .split(area);

    draw_resolution_status(f, &conflict_state_clone, inner_chunks[0]);
    draw_diff_view(f, app, &conflict_state_clone, inner_chunks[1]);
    draw_actions(f, &conflict_state_clone, inner_chunks[2]);
}

fn draw_resolution_status(f: &mut Frame, conflict_state: &ConflictState, area: Rect) {
    let resolution_text = match &conflict_state.resolution {
        Some(res) => {
            let text = match res {
                merge_command::ConflictResolution::TakeTarget => "Take TARGET version",
                merge_command::ConflictResolution::TakeSandbox => "Take SANDBOX version",
                merge_command::ConflictResolution::TakeBase => "Take BASE version",
                merge_command::ConflictResolution::Merged(_) => "Take BOTH (concatenated)",
                merge_command::ConflictResolution::Delete => "DELETE file",
            };
            Span::styled(
                format!("✓ {}", text),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )
        }
        None => Span::styled(
            "⚠ Unresolved - choose an action below",
            Style::default().fg(Color::Yellow),
        ),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(format!(" {} ", conflict_state.conflict.path.display()));

    let paragraph = Paragraph::new(Line::from(resolution_text)).block(block);

    f.render_widget(paragraph, area);
}

/// Represents a line in the diff view
#[derive(Debug, Clone)]
enum DiffLine {
    Context(String),  // Unchanged line (exists in both)
    Added(String),    // New line (only in this version)
    Removed(String),  // Deleted line (only in other version)
    Modified(String), // Changed line
}

/// Compute simple line-based diff between two contents
fn compute_line_diff(target: &str, sandbox: &str) -> (Vec<DiffLine>, Vec<DiffLine>) {
    let target_lines: Vec<&str> = target.lines().collect();
    let sandbox_lines: Vec<&str> = sandbox.lines().collect();

    let mut target_diff = Vec::new();
    let mut sandbox_diff = Vec::new();

    let max_len = target_lines.len().max(sandbox_lines.len());

    for i in 0..max_len {
        let target_line = target_lines.get(i).map(|s| *s);
        let sandbox_line = sandbox_lines.get(i).map(|s| *s);

        match (target_line, sandbox_line) {
            (Some(t), Some(s)) if t == s => {
                // Same line - context
                target_diff.push(DiffLine::Context(t.to_string()));
                sandbox_diff.push(DiffLine::Context(s.to_string()));
            }
            (Some(t), Some(s)) => {
                // Different content - modified
                target_diff.push(DiffLine::Modified(t.to_string()));
                sandbox_diff.push(DiffLine::Modified(s.to_string()));
            }
            (Some(t), None) => {
                // Line only in target - will be removed if sandbox chosen
                target_diff.push(DiffLine::Removed(t.to_string()));
            }
            (None, Some(s)) => {
                // Line only in sandbox - will be added if sandbox chosen
                sandbox_diff.push(DiffLine::Added(s.to_string()));
            }
            (None, None) => {}
        }
    }

    (target_diff, sandbox_diff)
}

fn draw_diff_view(f: &mut Frame, app: &mut App, conflict_state: &ConflictState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    let target_content = String::from_utf8_lossy(&conflict_state.target_content);
    let sandbox_content = String::from_utf8_lossy(&conflict_state.sandbox_content);

    let (target_diff, sandbox_diff) = compute_line_diff(&target_content, &sandbox_content);

    // Calculate visible area
    let inner_height = chunks[0].height.saturating_sub(2) as usize;
    let max_lines = target_diff.len().max(sandbox_diff.len());

    // Update max scroll
    app.diff_max_scroll = max_lines.saturating_sub(inner_height);

    // Draw target (left)
    draw_diff_panel(
        f,
        &target_diff,
        &format!(" {} (target) ", app.target_branch),
        Color::Green,
        chunks[0],
        app.diff_scroll,
        inner_height,
        true, // is_target
    );

    // Draw sandbox (right)
    draw_diff_panel(
        f,
        &sandbox_diff,
        &format!(" {} (sandbox) ", app.sandbox_name),
        Color::Yellow,
        chunks[1],
        app.diff_scroll,
        inner_height,
        false, // is_target
    );
}

fn draw_diff_panel(
    f: &mut Frame,
    diff_lines: &[DiffLine],
    title: &str,
    border_color: Color,
    area: Rect,
    scroll: usize,
    visible_height: usize,
    is_target: bool,
) {
    let line_num_width = diff_lines.len().to_string().len().max(3);

    let visible_lines: Vec<Line> = diff_lines
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible_height)
        .map(|(i, diff_line)| {
            let line_num = i + 1;
            let line_num_str = format!("{:>width$} ", line_num, width = line_num_width);

            match diff_line {
                DiffLine::Context(text) => Line::from(vec![
                    Span::styled(line_num_str, Style::default().fg(Color::DarkGray)),
                    Span::styled("  ", Style::default()),
                    Span::raw(truncate_line(
                        text,
                        area.width as usize - line_num_width - 5,
                    )),
                ]),
                DiffLine::Added(text) => Line::from(vec![
                    Span::styled(line_num_str, Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        "+ ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_line(text, area.width as usize - line_num_width - 5),
                        Style::default().bg(Color::Rgb(0, 60, 0)).fg(Color::Green),
                    ),
                ]),
                DiffLine::Removed(text) => Line::from(vec![
                    Span::styled(line_num_str, Style::default().fg(Color::DarkGray)),
                    Span::styled(
                        "- ",
                        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_line(text, area.width as usize - line_num_width - 5),
                        Style::default().bg(Color::Rgb(60, 0, 0)).fg(Color::Red),
                    ),
                ]),
                DiffLine::Modified(text) => {
                    let (bg_color, fg_color, symbol) = if is_target {
                        (Color::Rgb(60, 60, 0), Color::Yellow, "~")
                    } else {
                        (Color::Rgb(0, 60, 60), Color::Cyan, "~")
                    };
                    Line::from(vec![
                        Span::styled(line_num_str, Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{} ", symbol),
                            Style::default().fg(fg_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            truncate_line(text, area.width as usize - line_num_width - 5),
                            Style::default().bg(bg_color).fg(fg_color),
                        ),
                    ])
                }
            }
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);

    let paragraph = Paragraph::new(visible_lines).block(block);

    f.render_widget(paragraph, area);

    // Scrollbar
    if diff_lines.len() > visible_height {
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));

        let mut scrollbar_state = ScrollbarState::default()
            .content_length(diff_lines.len())
            .position(scroll);

        f.render_stateful_widget(
            scrollbar,
            area.inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

fn truncate_line(s: &str, max_width: usize) -> String {
    if s.len() > max_width {
        format!("{}...", &s[..max_width.saturating_sub(3)])
    } else {
        s.to_string()
    }
}

fn draw_actions(f: &mut Frame, conflict_state: &ConflictState, area: Rect) {
    let has_base = conflict_state.base_content.is_some();

    let actions = vec![
        Line::from(vec![
            Span::styled(
                "[t/1]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Target  "),
            Span::styled(
                "[s/2]",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Sandbox  "),
            if has_base {
                Span::styled(
                    "[b/3]",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled("[b/3]", Style::default().fg(Color::DarkGray))
            },
            Span::raw(" Base  "),
            Span::styled(
                "[a]",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Both"),
        ]),
        Line::from(vec![
            Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
            Span::raw(" scroll diff  "),
            Span::styled("PgUp/PgDn", Style::default().fg(Color::Cyan)),
            Span::raw(" page scroll"),
        ]),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(" Actions ");

    let paragraph = Paragraph::new(actions).block(block);

    f.render_widget(paragraph, area);
}

fn draw_help_bar(f: &mut Frame, app: &App, area: Rect) {
    let help_text = if app.all_resolved() {
        vec![
            Span::styled("Enter", Style::default().fg(Color::Green)),
            Span::raw(" confirm merge • "),
            Span::styled("Esc", Style::default().fg(Color::Red)),
            Span::raw(" cancel • "),
            Span::styled("?", Style::default().fg(Color::Cyan)),
            Span::raw(" help"),
        ]
    } else {
        vec![
            Span::styled("j/k", Style::default().fg(Color::Cyan)),
            Span::raw(" navigate • "),
            Span::styled("t/s/b/a", Style::default().fg(Color::Cyan)),
            Span::raw(" resolve • "),
            Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
            Span::raw(" scroll • "),
            Span::styled("Esc", Style::default().fg(Color::Red)),
            Span::raw(" cancel • "),
            Span::styled("?", Style::default().fg(Color::Cyan)),
            Span::raw(" help"),
        ]
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray));

    let paragraph = Paragraph::new(Line::from(help_text)).block(block);

    f.render_widget(paragraph, area);
}

fn draw_help_popup(f: &mut Frame, area: Rect) {
    let popup_area = centered_rect(60, 70, area);

    f.render_widget(Clear, popup_area);

    let help_text = vec![
        Line::from(Span::styled(
            "Merge Conflict Resolution",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Navigation:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  j/k       Move to next/prev conflict"),
        Line::from("  ↑/↓       Scroll diff view"),
        Line::from("  PgUp/PgDn Page scroll diff view"),
        Line::from("  Home/End  Scroll to top/bottom of diff"),
        Line::from(""),
        Line::from(Span::styled(
            "Resolution:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  t / 1     Take TARGET version"),
        Line::from("  s / 2     Take SANDBOX version"),
        Line::from("  b / 3     Take BASE version (if available)"),
        Line::from("  a         Take BOTH (concatenate)"),
        Line::from(""),
        Line::from(Span::styled(
            "Diff Legend:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("+ ", Style::default().fg(Color::Green)),
            Span::raw("Added line"),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("- ", Style::default().fg(Color::Red)),
            Span::raw("Removed line"),
        ]),
        Line::from(vec![
            Span::raw("  "),
            Span::styled("~ ", Style::default().fg(Color::Yellow)),
            Span::raw("Modified line"),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Actions:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  Enter     Confirm merge (when all resolved)"),
        Line::from("  Esc       Cancel merge"),
        Line::from("  ?         Toggle this help"),
        Line::from(""),
        Line::from(Span::styled(
            "Press any key to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Help ");

    let paragraph = Paragraph::new(help_text).block(block);

    f.render_widget(paragraph, popup_area);
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
