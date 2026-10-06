//! The left column: `RUNS` over `PROJECTS`, both reading straight from the snapshot.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::data::{self, RunProgress};
use crate::mascot;
use crate::theme;
use crate::ui::fit;

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    if app.lists_collapsed() {
        draw_collapsed(frame, area, app);
        return;
    }

    let runs = app.runs();
    let projects = app.projects();

    // The runs list takes what it needs, capped so the projects list and a little slack
    // always survive; projects take what they need from the remainder.
    let runs_height = (runs.len().max(1) as u16 + 2)
        .min(area.height.saturating_sub(4))
        .max(3);
    let projects_height = (projects.len().max(1) as u16 + 2)
        .min(area.height.saturating_sub(runs_height))
        .max(3);
    let [runs_area, projects_area, rest_area] = Layout::vertical([
        Constraint::Length(runs_height),
        Constraint::Length(projects_height),
        Constraint::Min(0),
    ])
    .areas(area);

    draw_runs(frame, runs_area, app, runs);
    draw_projects(frame, projects_area, app, projects);
    draw_mascot(frame, rest_area, app);
}

/// The mascot lives in the rows the two lists leave over, so it never moves the footer or
/// reflows a pane when it appears or disappears. An area too short for the full figure draws
/// nothing, matching the Ink `MIN_ROWS` clip.
fn draw_mascot(frame: &mut Frame, area: Rect, app: &App) {
    let lines = mascot::render(
        area.height as usize,
        app.animating(),
        app.paused(),
        app.mascot_frame(),
        mascot::now_millis(),
    );
    if lines.is_empty() {
        return;
    }
    frame.render_widget(Paragraph::new(lines), area);
}

/// `c` collapses both lists to a header plus count. The RUNS header keeps the selected run
/// visible, as the Ink collapse does, and the PROJECTS header keeps its count. Both are
/// bordered header-only blocks, so the right-hand detail pane keeps the same area and never
/// reflows.
fn draw_collapsed(frame: &mut Frame, area: Rect, app: &App) {
    let runs = app.runs();
    let projects = app.projects();
    let runs_title = match app.selected_run() {
        Some(run) => format!("RUNS ({})  ▸ {}", runs.len(), run.run),
        None => format!("RUNS ({})", runs.len()),
    };
    let runs_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            format!(" {} ", fit(&runs_title, 44)),
            theme::bold().fg(Color::White),
        ));
    let projects_block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            format!(" PROJECTS ({}) ", projects.len()),
            theme::bold().fg(Color::White),
        ));
    let [runs_area, projects_area, rest_area] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(runs_block, runs_area);
    frame.render_widget(projects_block, projects_area);
    draw_mascot(frame, rest_area, app);
}

fn draw_runs(frame: &mut Frame, area: Rect, app: &App, runs: &[RunProgress]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            format!(" RUNS ({}) ", runs.len()),
            theme::bold().fg(Color::White),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = if runs.is_empty() {
        vec![Line::from(Span::styled("  none", theme::dim()))]
    } else {
        runs.iter()
            .enumerate()
            .map(|(index, run)| run_row(run, index == app.selected(), app.spinner_frame()))
            .collect()
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_projects(frame: &mut Frame, area: Rect, app: &App, projects: &[crate::data::Project]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            format!(" PROJECTS ({}) ", projects.len()),
            theme::bold().fg(Color::White),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if projects.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled("  none", theme::dim()))),
            inner,
        );
        return;
    }

    let selected_project = app.selected_run().and_then(|run| run.project.as_deref());
    let lines: Vec<Line> = projects
        .iter()
        .map(|project| {
            // The selected run's project stays bright; the rest recede.
            let muted = Some(project.id.as_str()) != selected_project;
            let style = if muted { theme::dim() } else { Style::new() };
            Line::from(vec![
                Span::raw("  "),
                Span::styled(project.id.clone(), style),
                Span::raw(" "),
                Span::styled(
                    project.status.clone(),
                    Style::new().fg(theme::project_colour(&project.status)),
                ),
                Span::styled(
                    format!(" {}h {}i", project.hints(), project.intents()),
                    style,
                ),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn run_row(run: &RunProgress, selected: bool, spinner: &str) -> Line<'static> {
    let state = run.state();
    let colour = theme::state_colour(state);
    let mut spans = Vec::new();

    spans.push(if selected {
        Span::styled("▸ ", theme::accent())
    } else {
        Span::raw("  ")
    });

    if run.is_animating() {
        spans.push(Span::styled(spinner.to_string(), Style::new().fg(colour)));
        spans.push(Span::raw(" "));
    } else if run.paused {
        spans.push(Span::styled("‖ ", Style::new().fg(colour)));
    } else {
        spans.push(Span::styled("○ ", Style::new().fg(colour)));
    }

    let name_style = if selected {
        theme::bold().fg(Color::White)
    } else {
        Style::new()
    };
    spans.push(Span::styled(fit(&run.run, 23), name_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(fit(state, 9), Style::new().fg(colour)));
    spans.push(Span::styled(
        format!(
            "{:>6}",
            data::elapsed(run.start_time.as_deref(), run.end_time.as_deref())
        ),
        theme::dim(),
    ));
    Line::from(spans)
}
