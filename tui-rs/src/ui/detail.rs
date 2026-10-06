//! The detail pane: the selected run's `STRIX` block, its `CAIRN` block and a
//! `TELEMETRY` block. Everything here comes from the snapshot or local `/proc` reads;
//! no pane adds a CLI call of its own.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Sparkline, Wrap};
use ratatui::Frame;

use crate::app::App;
use crate::data::{self, RunProgress, TodoDetail};
use crate::theme;
use crate::ui::{progress_bar, SEVERITY_ORDER};

pub fn draw(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(error) = app.error() {
        draw_error(frame, area, error);
        return;
    }
    let Some(run) = app.selected_run() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "no runs yet — start one with `triad engage`",
                theme::dim(),
            ))),
            area,
        );
        return;
    };

    // The three blocks stack from the top, each sized to its content, with any slack left
    // below them. Sizing Strix to its lines keeps a sparse run from stranding the Cairn
    // and telemetry blocks at the bottom of the pane.
    let strix = strix_lines(run);
    let cairn_height: u16 = if run.project.is_some() { 6 } else { 3 };
    let telemetry_height: u16 = 6;
    let fixed = cairn_height + telemetry_height;
    let strix_height = (strix.len() as u16 + 2)
        .min(area.height.saturating_sub(fixed))
        .max(6);
    let [strix_area, cairn_area, telemetry_area, _] = Layout::vertical([
        Constraint::Length(strix_height),
        Constraint::Length(cairn_height),
        Constraint::Length(telemetry_height),
        Constraint::Min(0),
    ])
    .areas(area);

    draw_strix(frame, strix_area, strix);
    draw_cairn(frame, cairn_area, run, app);
    draw_telemetry(frame, telemetry_area, app);
}

fn draw_error(frame: &mut Frame, area: Rect, error: &str) {
    let block = panel("ERROR");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(error.to_string(), Style::new().fg(Color::Red))),
            Line::from(Span::styled(
                "keeping the last good snapshot; the next poll retries",
                theme::dim(),
            )),
        ])
        .wrap(Wrap { trim: false }),
        inner,
    );
}

fn draw_strix(frame: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let block = panel("STRIX");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The reader sees the whole Strix block at a glance: run, state, agents, todos,
/// findings, tokens and the now line, in the order the work happens.
fn strix_lines(run: &RunProgress) -> Vec<Line<'static>> {
    let state = run.state();
    let live = run.is_animating();

    let mut lines = vec![
        label_line("run", vec![Span::raw(run.run.clone())]),
        label_line(
            "state",
            vec![
                Span::styled(
                    state.to_string(),
                    Style::new().fg(theme::state_colour(state)),
                ),
                Span::styled(
                    format!(
                        "  {}  pid {}",
                        data::elapsed(run.start_time.as_deref(), run.end_time.as_deref()),
                        run.pid
                            .map_or_else(|| "-".to_string(), |pid| pid.to_string()),
                    ),
                    theme::dim(),
                ),
            ],
        ),
    ];

    let mut agents = vec![Span::styled(
        format!(
            "[{}] {}/{} done",
            progress_bar(run.agents.completed, run.agents.total, 10),
            run.agents.completed,
            run.agents.total
        ),
        bar_style(
            run.agents.completed,
            run.agents.total,
            !run.agents.running.is_empty() && live,
        ),
    )];
    let mut agent_extra = String::new();
    if !run.agents.running.is_empty() {
        agent_extra.push_str(&format!("  {} working", run.agents.running.len()));
    }
    if run.agents.failed > 0 {
        agent_extra.push_str(&format!("  {} failed", run.agents.failed));
    }
    if !agent_extra.is_empty() {
        agents.push(Span::styled(agent_extra, theme::dim()));
    }
    lines.push(label_line("agents", agents));

    lines.push(label_line(
        "todos",
        vec![
            Span::styled(
                format!(
                    "[{}] {}/{} done",
                    progress_bar(run.todos.done, run.todos.total, 10),
                    run.todos.done,
                    run.todos.total
                ),
                bar_style(
                    run.todos.done,
                    run.todos.total,
                    run.todos.in_progress > 0 && live,
                ),
            ),
            Span::styled(
                format!("  {} in progress", run.todos.in_progress),
                theme::dim(),
            ),
        ],
    ));

    lines.push(label_line(
        "findings",
        vec![
            Span::styled(
                run.findings.to_string(),
                Style::new().fg(if run.findings > 0 {
                    Color::Green
                } else {
                    Color::Gray
                }),
            ),
            Span::styled(
                format!("  gaps {}  notes {}", run.coverage_gaps, run.notes),
                theme::dim(),
            ),
        ],
    ));

    let mut severity: Vec<Span> = Vec::new();
    for name in SEVERITY_ORDER {
        let count = run.severity(name);
        if count == 0 {
            continue;
        }
        if !severity.is_empty() {
            severity.push(Span::styled(" · ", theme::dim()));
        }
        severity.push(Span::styled(
            format!("{name} {count}"),
            Style::new().fg(theme::severity_colour(name)),
        ));
    }
    if severity.is_empty() {
        severity.push(Span::styled("no findings", theme::dim()));
    }
    lines.push(label_line("", severity));

    lines.push(label_line(
        "tokens",
        vec![
            Span::raw(format!(
                "{} in / {} out",
                data::human(run.usage.input_tokens),
                data::human(run.usage.output_tokens)
            )),
            Span::styled(
                format!(
                    "  {} requests",
                    run.usage
                        .requests
                        .map_or_else(|| "-".to_string(), |r| r.to_string())
                ),
                theme::dim(),
            ),
        ],
    ));

    lines.push(label_line("cost", cost_spans(run)));

    let in_progress: Vec<&TodoDetail> = run
        .todos_detail
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|todo| todo.status.as_deref() == Some("in_progress"))
        .take(3)
        .collect();
    if !run.agents.running.is_empty() || !in_progress.is_empty() {
        let now = if run.agents.running.is_empty() {
            vec![Span::styled("no agents running", theme::dim())]
        } else {
            vec![Span::styled(
                run.agents
                    .running
                    .iter()
                    .take(3)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
                theme::accent(),
            )]
        };
        lines.push(label_line("now", now));
        for todo in in_progress {
            let title = todo.title.clone().unwrap_or_else(|| todo.id.clone());
            lines.push(label_line(
                "",
                vec![Span::styled(format!("· {title}"), theme::dim())],
            ));
        }
    }

    lines
}

fn draw_cairn(frame: &mut Frame, area: Rect, run: &RunProgress, app: &App) {
    let block = panel("CAIRN");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    if let Some(project) = app.selected_project() {
        lines.push(label_line(
            "project",
            vec![
                Span::raw(project.id.clone()),
                Span::styled(
                    format!("  {}", project.status),
                    Style::new().fg(theme::project_colour(&project.status)),
                ),
            ],
        ));
        lines.push(label_line(
            "graph",
            vec![Span::raw(format!(
                "{} facts · {} hints · {} intents · {} open",
                project.facts(),
                project.hints(),
                project.intents(),
                project.open()
            ))],
        ));
        if let Some(fed) = run.project_fed_at.as_deref() {
            lines.push(label_line(
                "fed",
                vec![Span::raw(data::relative_age(Some(fed)))],
            ));
        }
        lines.push(dispatcher_line(
            app,
            Some((project.unclaimed(), project.working())),
        ));
    } else if let Some(linked) = run.project.as_deref() {
        // The link is on disk but the project list is empty because Cairn is down. The run
        // is linked; saying otherwise sends the user to re-create a project that exists.
        lines.push(label_line(
            "project",
            vec![
                Span::raw(linked.to_string()),
                Span::styled("  linked", Style::new().fg(Color::Yellow)),
            ],
        ));
        lines.push(label_line(
            "graph",
            vec![Span::styled(
                "unavailable — Cairn is not answering",
                Style::new().fg(Color::Yellow),
            )],
        ));
        if let Some(fed) = run.project_fed_at.as_deref() {
            lines.push(label_line(
                "fed",
                vec![Span::raw(data::relative_age(Some(fed)))],
            ));
        }
        lines.push(dispatcher_line(app, None));
    } else {
        lines.push(label_line(
            "project",
            vec![
                Span::styled("none linked — run ", theme::dim()),
                Span::styled("triad engage", theme::accent()),
                Span::styled(" to link one", theme::dim()),
            ],
        ));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_telemetry(frame: &mut Frame, area: Rect, app: &App) {
    let block = panel("TELEMETRY");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let run_pid = app.selected_run().and_then(|run| run.pid);
    let proc_line = process_line("run proc", run_pid, app);
    let dispatcher = match app.dispatcher_pid() {
        Some(pid) if app.dispatcher_alive() => process_line("dispatcher", Some(pid), app),
        _ => label_line("dispatcher", vec![Span::styled("stopped", theme::dim())]),
    };
    let poll_line = label_line(
        "poll",
        vec![Span::styled(
            format!(
                "{}s interval · {} samples",
                app.interval_secs(),
                app.poll_seq()
            ),
            theme::dim(),
        )],
    );

    let [text_area, spark_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(1)]).areas(inner);
    frame.render_widget(
        Paragraph::new(vec![proc_line, dispatcher, poll_line]),
        text_area,
    );

    let [label_area, graph_area] =
        Layout::horizontal([Constraint::Length(4), Constraint::Min(0)]).areas(spark_area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled("cpu ", theme::dim()))),
        label_area,
    );
    frame.render_widget(
        Sparkline::default()
            .data(app.cpu_history())
            .style(Style::new().fg(Color::Cyan)),
        graph_area,
    );
}

fn process_line(label: &str, pid: Option<i64>, app: &App) -> Line<'static> {
    match pid {
        Some(pid) => label_line(
            label,
            vec![
                Span::raw(pid.to_string()),
                Span::styled(
                    format!(
                        "  {}  {}",
                        cpu_text(app.proc_cpu(pid)),
                        data::human_kb(app.proc_rss(pid))
                    ),
                    Style::new().fg(Color::Cyan),
                ),
            ],
        ),
        None => label_line(label, vec![Span::styled("no live process", theme::dim())]),
    }
}

fn cpu_text(cpu: Option<f64>) -> String {
    format!(
        "{:>4}% cpu",
        cpu.map_or_else(|| "-".to_string(), |value| format!("{value:.0}"))
    )
}

fn cost_spans(run: &RunProgress) -> Vec<Span<'static>> {
    match run.cost_usd {
        Some(cost) => {
            let mut spans = vec![Span::raw(format!("${cost:.4}"))];
            if let Some(turns) = run.turns {
                if turns > 0 {
                    spans.push(Span::styled(
                        format!("  ${:.4}/turn", cost / turns as f64),
                        theme::dim(),
                    ));
                }
            }
            spans
        }
        None => vec![Span::styled("not reported", theme::dim())],
    }
}

fn bar_style(done: i64, total: i64, working: bool) -> Style {
    if total > 0 && done >= total {
        Style::new().fg(Color::Green)
    } else if working {
        Style::new().fg(Color::Cyan)
    } else {
        theme::dim()
    }
}

fn dispatcher_line(app: &App, counts: Option<(i64, i64)>) -> Line<'static> {
    let mut spans: Vec<Span> = vec![if app.dispatcher_alive() {
        Span::styled("up", theme::dim())
    } else {
        Span::styled(
            "DOWN — nothing advances until it is up",
            Style::new().fg(Color::Red),
        )
    }];
    if let Some((unclaimed, working)) = counts {
        spans.push(Span::styled(
            format!("  {unclaimed} unclaimed  {working} working"),
            theme::dim(),
        ));
    }
    label_line("dispatcher", spans)
}

fn panel(title: &'static str) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_style(theme::dim())
        .title(Span::styled(
            format!(" {title} "),
            theme::bold().fg(Color::White),
        ))
}

/// A fixed label column so values line up; an empty label indents a continuation line.
fn label_line(label: &str, spans: Vec<Span<'static>>) -> Line<'static> {
    let mut out = vec![Span::styled(format!("{label:<11}"), theme::dim())];
    out.extend(spans);
    Line::from(out)
}
