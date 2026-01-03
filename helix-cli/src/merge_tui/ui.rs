use merge_command::ConflictType;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap},
    Frame,
};

use crate::merge_command;

use super::app::{App, ConflictState};

pub fn draw(f: &mut Frame, app: &App) {
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

fn draw_main(f: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(area);

    draw_conflict_list(f, app, chunks[0]);
    draw_conflict_detail(f, app, chunks[1]);
}

fn draw_conflict_list(f: &mut Frame, app: &App, area: Rect) {
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
            let path_short = if path.len() > 25 {
                format!("...{}", &path[path.len() - 22..])
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

    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(" Conflicts "),
    );

    f.render_widget(list, area);
}

fn draw_conflict_detail(f: &mut Frame, app: &App, area: Rect) {
    let Some(conflict_state) = app.selected_conflict() else {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Blue))
            .title(" No conflicts ");
        f.render_widget(block, area);
        return;
    };

    let inner_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Resolution status
            Constraint::Min(5),    // Content diff
            Constraint::Length(5), // Actions
        ])
        .split(area);

    // Resolution status
    draw_resolution_status(f, conflict_state, inner_chunks[0]);

    // Content diff
    draw_content_diff(f, app, conflict_state, inner_chunks[1]);

    // Actions
    draw_actions(f, conflict_state, inner_chunks[2]);
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

fn draw_content_diff(f: &mut Frame, app: &App, conflict_state: &ConflictState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    // Target content (left)
    let target_lines: Vec<Line> = String::from_utf8_lossy(&conflict_state.target_content)
        .lines()
        .take(20)
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();

    let target_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green))
        .title(format!(" {} (target) ", app.target_branch));

    let target_para = Paragraph::new(target_lines)
        .block(target_block)
        .wrap(Wrap { trim: false });

    f.render_widget(target_para, chunks[0]);

    // Sandbox content (right)
    let sandbox_lines: Vec<Line> = String::from_utf8_lossy(&conflict_state.sandbox_content)
        .lines()
        .take(20)
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();

    let sandbox_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(format!(" {} (sandbox) ", app.sandbox_name));

    let sandbox_para = Paragraph::new(sandbox_lines)
        .block(sandbox_block)
        .wrap(Wrap { trim: false });

    f.render_widget(sandbox_para, chunks[1]);
}

fn draw_actions(f: &mut Frame, conflict_state: &ConflictState, area: Rect) {
    let has_base = conflict_state.base_content.is_some();

    let actions = vec![
        Span::styled("[t/1]", Style::default().fg(Color::Green)),
        Span::raw(" Target  "),
        Span::styled("[s/2]", Style::default().fg(Color::Yellow)),
        Span::raw(" Sandbox  "),
        if has_base {
            Span::styled("[b/3]", Style::default().fg(Color::Cyan))
        } else {
            Span::styled("[b/3]", Style::default().fg(Color::DarkGray))
        },
        Span::raw(" Base  "),
        Span::styled("[a]", Style::default().fg(Color::Magenta)),
        Span::raw(" Both  "),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Blue))
        .title(" Actions ");

    let paragraph = Paragraph::new(Line::from(actions)).block(block);

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
        Line::from("  j/↓       Move to next conflict"),
        Line::from("  k/↑       Move to previous conflict"),
        Line::from("  n         Next unresolved conflict"),
        Line::from("  p         Previous unresolved conflict"),
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
            "Actions:",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from("  Enter     Confirm merge (when all resolved)"),
        Line::from("  Esc       Cancel merge"),
        Line::from("  Tab / e   Toggle expanded view"),
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
