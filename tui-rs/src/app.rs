//! Application state and key handling. The drawing code in `ui` reads this and nothing
//! else, so no widget has to know how a snapshot is fetched or a key is routed.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::data::{
    self, CairnData, CairnLogs, DataError, ProgressDetail, Project, ProjectGraph, RunProgress,
    Snapshot,
};
use crate::form::{FormState, LAST_ROW};
use crate::graph::{self, Layout};

/// The spinner cadence: ten frames, roughly one revolution per second. The frame only
/// advances while something is live, so an idle dashboard never redraws on its own.
pub const TICK: Duration = Duration::from_millis(100);
/// The frontier marker's cadence, as the brief asks: roughly 200ms per step.
pub const PULSE_TICK: Duration = Duration::from_millis(200);
const HISTORY_LEN: usize = 60;
const CLK_TCK: f64 = 100.0; // Linux userspace default
/// A poll result must be noticed promptly, but an idle loop should still sleep. This caps
/// the event wait so a completed background poll is never stuck behind a long blocking read.
const MAX_WAIT: Duration = Duration::from_millis(150);
/// One line of the graph block is the legend, under the canvas.
pub const CAIRN_LEGEND_ROWS: u16 = 1;

/// Which page owns the body. The dashboard is the Stage 1 view; the Cairn page is the
/// graph over the logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Dashboard,
    Form,
    Cairn,
    Detail,
}

/// A submit's outcome, carried back from the worker so the UI thread never blocks on the
/// CLI. `Started` carries the line to report, `Failed` the CLI's own error text.
pub enum SubmitOutcome {
    Started(String),
    Failed(String),
}

/// How a status line reads: an in-progress note, a success, or a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageKind {
    Info,
    Ok,
    Err,
}

/// The dashboard's one status line, the Ink app's `message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub kind: MessageKind,
}

impl Message {
    pub fn info(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: MessageKind::Info,
        }
    }
    pub fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: MessageKind::Ok,
        }
    }
    pub fn err(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: MessageKind::Err,
        }
    }
}

/// On the dashboard, what `enter` acts on. `tab` toggles it, as the Ink list view does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Run,
    Cairn,
}

/// Which half of the Cairn page the scroll keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CairnPane {
    Graph,
    Logs,
}

/// Which half of the Strix modal the scroll keys act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailPane {
    Findings,
    Stream,
}

/// How the Strix modal's pane area splits between FINDINGS and the STREAM, in rows. The
/// top block takes about forty percent (at least six rows), the stream keeps the majority,
/// as the Ink modal settled on. A pure function so the drawing code and the follow-the-tail
/// math cannot disagree about how tall the stream window is.
pub fn detail_pane_split(panes: u16) -> (u16, u16) {
    if panes == 0 {
        return (0, 0);
    }
    let upper = ((panes as u32 * 40) / 100).max(6).min(panes as u32) as u16;
    (upper, panes - upper)
}

/// How the Cairn body splits into the graph block and the logs block, in rows. A pure
/// function so the drawing code and the scroll math cannot disagree about the pane sizes.
pub fn cairn_split_body(body: u16) -> (u16, u16) {
    if body < 6 {
        return (body, 0);
    }
    let graph = ((body as u32 * 62) / 100).max(5).min(body as u32) as u16;
    (graph, body - graph)
}

/// The canvas rows available inside the graph block (borders and legend removed).
pub fn cairn_graph_canvas_height(rows: u16) -> u16 {
    let (graph, _) = cairn_split_body(rows.saturating_sub(2));
    graph.saturating_sub(2 + CAIRN_LEGEND_ROWS).max(1)
}

/// The usable rows inside the logs block (borders removed).
pub fn cairn_log_inner_height(rows: u16) -> u16 {
    let (_, logs) = cairn_split_body(rows.saturating_sub(2));
    logs.saturating_sub(2).max(1)
}

/// A point sample from `/proc/<pid>`; CPU is a delta between two of these.
#[derive(Debug, Clone, Copy)]
struct ProcSample {
    ticks: i64,
    rss_kb: i64,
    at: Instant,
}

pub struct App {
    snapshot: Option<Snapshot>,
    error: Option<String>,
    selected: usize,
    should_quit: bool,
    interval: Duration,
    spinner: u64,
    poll_seq: u64,
    next_poll: Instant,
    last_tick: Instant,
    poll_rx: Option<Receiver<Result<Snapshot, DataError>>>,
    proc_prev: HashMap<i64, ProcSample>,
    proc_cpu: HashMap<i64, Option<f64>>,
    proc_rss: HashMap<i64, Option<i64>>,
    cpu_history: Vec<u64>,
    history_dir: String,

    // --- the new-engagement form ----------------------------------------------------
    form: FormState,
    submit_rx: Option<Receiver<SubmitOutcome>>,
    message: Option<Message>,

    // --- the Cairn page -------------------------------------------------------------
    page: Page,
    target: Target,
    cairn_pane: CairnPane,
    graph: Option<ProjectGraph>,
    graph_error: Option<String>,
    graph_layout: Option<Layout>,
    graph_scroll: u16,
    logs: Option<CairnLogs>,
    logs_error: Option<String>,
    log_scroll: usize,
    log_follow: bool,
    log_new: usize,
    log_prev_len: usize,
    log_prev_last: String,
    log_reset: bool,
    cairn_rx: Option<Receiver<CairnData>>,
    next_cairn: Instant,
    pulse: u64,
    last_pulse: Instant,
    term_cols: u16,
    term_rows: u16,

    // --- the Strix detail modal -----------------------------------------------------
    detail: Option<ProgressDetail>,
    detail_error: Option<String>,
    detail_has_db: bool,
    detail_pane: DetailPane,
    detail_scroll: usize,
    detail_findings_scroll: usize,
    detail_follow: bool,
    detail_new: usize,
    detail_prev_len: usize,
    detail_prev_max: usize,
    detail_baseline_id: i64,
    detail_reset: bool,
    detail_target: Option<(String, String)>,
    detail_rx: Option<Receiver<Result<ProgressDetail, DataError>>>,
    next_detail: Instant,
}

impl App {
    pub fn new(interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            snapshot: None,
            error: None,
            selected: 0,
            should_quit: false,
            interval,
            spinner: 0,
            poll_seq: 0,
            next_poll: now,
            last_tick: now,
            poll_rx: None,
            proc_prev: HashMap::new(),
            proc_cpu: HashMap::new(),
            proc_rss: HashMap::new(),
            cpu_history: Vec::new(),
            history_dir: String::new(),
            form: FormState::blank(),
            submit_rx: None,
            message: None,
            page: Page::Dashboard,
            target: Target::Run,
            cairn_pane: CairnPane::Graph,
            graph: None,
            graph_error: None,
            graph_layout: None,
            graph_scroll: 0,
            logs: None,
            logs_error: None,
            log_scroll: 0,
            log_follow: true,
            log_new: 0,
            log_prev_len: 0,
            log_prev_last: String::new(),
            log_reset: true,
            cairn_rx: None,
            next_cairn: now,
            pulse: 0,
            last_pulse: now,
            term_cols: 132,
            term_rows: 42,
            detail: None,
            detail_error: None,
            detail_has_db: true,
            detail_pane: DetailPane::Stream,
            detail_scroll: 0,
            detail_findings_scroll: 0,
            detail_follow: true,
            detail_new: 0,
            detail_prev_len: 0,
            detail_prev_max: 0,
            detail_baseline_id: 0,
            detail_reset: true,
            detail_target: None,
            detail_rx: None,
            next_detail: now,
        }
    }

    // --- reads the UI needs ---------------------------------------------------------

    pub fn snapshot(&self) -> Option<&Snapshot> {
        self.snapshot.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn runs(&self) -> &[RunProgress] {
        self.snapshot.as_ref().map_or(&[], |s| s.runs.as_slice())
    }

    pub fn projects(&self) -> &[Project] {
        self.snapshot
            .as_ref()
            .map_or(&[], |s| s.cairn.projects.as_slice())
    }

    pub fn dispatcher_alive(&self) -> bool {
        self.snapshot.as_ref().is_some_and(|s| s.dispatcher.alive)
    }

    pub fn dispatcher_pid(&self) -> Option<i64> {
        self.snapshot.as_ref().and_then(|s| s.dispatcher.pid)
    }

    /// The selected run, clamped: a refresh that shrinks the list must not leave the
    /// selection pointing past the end.
    pub fn selected_run(&self) -> Option<&RunProgress> {
        let runs = self.runs();
        runs.get(self.selected.min(runs.len().saturating_sub(1)))
    }

    pub fn selected_project(&self) -> Option<&Project> {
        let wanted = self.selected_run()?.project.as_deref()?;
        self.projects().iter().find(|p| p.id == wanted)
    }

    /// The spinner runs only while at least one run is actually scanning.
    pub fn animating(&self) -> bool {
        self.runs().iter().any(RunProgress::is_animating)
    }

    pub fn spinner_frame(&self) -> &'static str {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let index = (self.spinner % frames.len() as u64) as usize;
        frames[index]
    }

    pub fn poll_seq(&self) -> u64 {
        self.poll_seq
    }

    pub fn interval_secs(&self) -> u64 {
        self.interval.as_secs().max(1)
    }

    pub fn form(&self) -> &FormState {
        &self.form
    }

    pub fn message(&self) -> Option<&Message> {
        self.message.as_ref()
    }

    /// The footer needs a second row on the dashboard while a submit status line is showing,
    /// the way the Ink footer stacks `message` above its key line. Every other page is one
    /// line.
    pub fn footer_height(&self) -> u16 {
        if self.page == Page::Dashboard && self.message.is_some() {
            2
        } else {
            1
        }
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn cpu_history(&self) -> &[u64] {
        &self.cpu_history
    }

    /// Whether the CPU history has anything worth plotting. An empty or all-zero history
    /// leaves a bare `cpu` label over nothing, so the row is dropped instead.
    pub fn cpu_signal(&self) -> bool {
        self.cpu_history.iter().any(|&value| value > 0)
    }

    pub fn proc_cpu(&self, pid: i64) -> Option<f64> {
        self.proc_cpu.get(&pid).copied().flatten()
    }

    pub fn proc_rss(&self, pid: i64) -> Option<i64> {
        self.proc_rss.get(&pid).copied().flatten()
    }

    // --- Cairn page reads -----------------------------------------------------------

    pub fn page(&self) -> Page {
        self.page
    }

    pub fn target(&self) -> Target {
        self.target
    }

    pub fn cairn_pane(&self) -> CairnPane {
        self.cairn_pane
    }

    pub fn graph(&self) -> Option<&ProjectGraph> {
        self.graph.as_ref()
    }

    pub fn graph_error(&self) -> Option<&str> {
        self.graph_error.as_deref()
    }

    pub fn graph_layout(&self) -> Option<&Layout> {
        self.graph_layout.as_ref()
    }

    pub fn graph_max_scroll(&self) -> u16 {
        let visible = cairn_graph_canvas_height(self.term_rows);
        self.graph_layout
            .as_ref()
            .map_or(0, |layout| layout.max_scroll(visible))
    }

    pub fn logs(&self) -> Option<&CairnLogs> {
        self.logs.as_ref()
    }

    pub fn logs_error(&self) -> Option<&str> {
        self.logs_error.as_deref()
    }

    pub fn log_scroll(&self) -> usize {
        self.log_scroll
    }

    pub fn log_follow(&self) -> bool {
        self.log_follow
    }

    pub fn log_new(&self) -> usize {
        self.log_new
    }

    pub fn pulse(&self) -> u64 {
        self.pulse
    }

    /// The frontier pulses only for an active project with open work, the gate the Ink
    /// version uses. A completed or stopped project has nothing to pulse.
    pub fn pulse_active(&self) -> bool {
        self.page == Page::Cairn
            && self
                .graph
                .as_ref()
                .is_some_and(|graph| graph.project.status == "active" && graph.counts.open > 0)
    }

    /// Record the terminal size; a change re-flows the graph and must reset the scroll so
    /// the picture never jumps to a stale offset. Returns whether anything changed.
    pub fn set_size(&mut self, cols: u16, rows: u16) -> bool {
        if cols == self.term_cols && rows == self.term_rows {
            return false;
        }
        self.term_cols = cols;
        self.term_rows = rows;
        self.recompute_layout(true);
        self.clamp_log_scroll();
        self.clamp_detail_scroll();
        true
    }

    // --- the loop's timing hooks ----------------------------------------------------

    /// Start a poll when one is due and none is already in flight. The fetch runs on a
    /// worker thread so a slow CLI can never freeze the keys.
    pub fn start_poll_if_due(&mut self, now: Instant) {
        if self.poll_rx.is_some() || now < self.next_poll {
            return;
        }
        self.next_poll = now + self.interval;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(data::fetch_snapshot());
        });
        self.poll_rx = Some(rx);
    }

    /// Collect a finished poll, if any. Returns whether anything changed on screen.
    pub fn pump(&mut self, now: Instant) -> bool {
        let Some(rx) = &self.poll_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.poll_rx = None;
                self.apply(result);
                // Schedule the next poll from completion, not from start, so a slow call
                // does not immediately re-fire.
                self.next_poll = now + self.interval;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.poll_rx = None;
                self.error = Some("the CLI poller stopped unexpectedly".to_string());
                self.next_poll = now + self.interval;
                true
            }
        }
    }

    /// Advance the spinner while something is live, and the frontier marker while the
    /// project is active. Returns whether to redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        let mut redraw = false;
        if self.animating() && now >= self.last_tick + TICK {
            self.spinner = self.spinner.wrapping_add(1);
            self.last_tick = now;
            redraw = true;
        }
        if self.pulse_active() && now >= self.last_pulse + PULSE_TICK {
            self.pulse = self.pulse.wrapping_add(1);
            self.last_pulse = now;
            redraw = true;
        }
        redraw
    }

    /// How long the event wait may block before timers want attention again.
    pub fn wait_hint(&self, now: Instant) -> Duration {
        let mut wait = self.next_poll.saturating_duration_since(now);
        if self.page == Page::Cairn {
            wait = wait.min(self.next_cairn.saturating_duration_since(now));
        }
        if self.page == Page::Detail {
            wait = wait.min(self.next_detail.saturating_duration_since(now));
        }
        if self.animating() {
            let until_tick = (self.last_tick + TICK).saturating_duration_since(now);
            wait = wait.min(until_tick);
        }
        if self.pulse_active() {
            let until_pulse = (self.last_pulse + PULSE_TICK).saturating_duration_since(now);
            wait = wait.min(until_pulse);
        }
        wait.min(MAX_WAIT)
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    // --- the Cairn page's fetch lifecycle -------------------------------------------

    /// Open the Cairn page: reset its scroll and follow state and fetch immediately, so
    /// reopen starts at the top of a fresh fetch rather than an old offset.
    pub fn open_cairn(&mut self) {
        self.page = Page::Cairn;
        self.cairn_pane = CairnPane::Graph;
        self.graph = None;
        self.graph_error = None;
        self.graph_layout = None;
        self.graph_scroll = 0;
        self.logs = None;
        self.logs_error = None;
        self.log_scroll = 0;
        self.log_follow = true;
        self.log_new = 0;
        self.log_reset = true;
        self.pulse = 0;
        self.next_cairn = Instant::now();
        self.start_cairn_if_due(Instant::now());
    }

    pub fn close_cairn(&mut self) {
        self.page = Page::Dashboard;
        self.cairn_rx = None;
    }

    /// Start a Cairn fetch when one is due and none is in flight. Both payloads travel on
    /// one worker thread so a slow CLI can never freeze the keys.
    pub fn start_cairn_if_due(&mut self, now: Instant) {
        if self.page != Page::Cairn || self.cairn_rx.is_some() || now < self.next_cairn {
            return;
        }
        self.next_cairn = now + self.interval;
        let linked = self.selected_run().and_then(|run| run.project.clone());
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(data::fetch_cairn(linked));
        });
        self.cairn_rx = Some(rx);
    }

    /// Refetch because the user pressed `r`, without waiting for the interval.
    pub fn refetch_cairn(&mut self) {
        self.next_cairn = Instant::now();
        self.start_cairn_if_due(Instant::now());
    }

    /// Collect a finished Cairn fetch. Returns whether anything changed on screen.
    pub fn pump_cairn(&mut self, now: Instant) -> bool {
        if self.page != Page::Cairn {
            self.cairn_rx = None;
            return false;
        }
        let Some(rx) = &self.cairn_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(data) => {
                self.cairn_rx = None;
                self.apply_cairn(data);
                self.next_cairn = now + self.interval;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.cairn_rx = None;
                self.graph_error = Some("the Cairn fetch stopped unexpectedly".to_string());
                self.next_cairn = now + self.interval;
                true
            }
        }
    }

    fn apply_cairn(&mut self, data: CairnData) {
        match data.graph {
            None => {
                self.graph = None;
                self.graph_error = None;
            }
            Some(Ok(graph)) => {
                self.graph = Some(graph);
                self.graph_error = None;
            }
            Some(Err(error)) => {
                self.graph = None;
                self.graph_error = Some(error.to_string());
            }
        }
        self.recompute_layout(false);

        match data.logs {
            Ok(logs) => {
                self.logs_error = None;
                self.apply_log_tail(logs);
            }
            Err(error) => {
                self.logs = None;
                self.logs_error = Some(error.to_string());
                self.log_new = 0;
            }
        }
    }

    /// Follow-the-tail, the same intent the Ink log pane has: stay pinned to the bottom
    /// unless the reader scrolled up, and then count what arrived while they read.
    fn apply_log_tail(&mut self, logs: CairnLogs) {
        let len = logs.lines.len();
        let last = logs.lines.last().cloned().unwrap_or_default();
        let height = cairn_log_inner_height(self.term_rows) as usize;

        if self.log_reset {
            self.log_reset = false;
            self.log_prev_len = len;
            self.log_prev_last = last;
            self.log_follow = true;
            self.log_new = 0;
            self.log_scroll = len.saturating_sub(height);
            self.logs = Some(logs);
            return;
        }

        let added = len as i64 - self.log_prev_len as i64;
        let changed = last != self.log_prev_last;
        let was_bottom = self.log_scroll + height >= self.log_prev_len;
        self.log_prev_len = len;
        self.log_prev_last = last;

        if self.log_follow && was_bottom {
            self.log_scroll = len.saturating_sub(height);
            self.log_new = 0;
        } else {
            if self.log_follow {
                self.log_follow = false;
            }
            if added > 0 || changed {
                self.log_new += if added > 0 { added as usize } else { 1 };
            }
            if added < 0 {
                // The window slid: lines left the top, so keep the same lines on screen.
                self.log_scroll = self.log_scroll.saturating_sub((-added) as usize);
            }
        }
        self.logs = Some(logs);
    }

    /// Rebuild the graph layout from the current graph and terminal size. A resize
    /// re-clamps the scroll; a fresh graph leaves it alone unless there is no layout yet.
    fn recompute_layout(&mut self, resize: bool) {
        if resize {
            self.graph_scroll = 0;
        }
        let Some(graph) = &self.graph else {
            self.graph_layout = None;
            self.graph_scroll = 0;
            return;
        };
        let width = self.term_cols.saturating_sub(2).max(1) as f64;
        let height = cairn_graph_canvas_height(self.term_rows) as f64;
        self.graph_layout = Some(graph::layout(graph, width, height));
        let max = self.graph_max_scroll();
        self.graph_scroll = self.graph_scroll.min(max);
    }

    /// The log pane's scroll window, clamped to the tail.
    fn clamp_log_scroll(&mut self) {
        let Some(logs) = &self.logs else {
            self.log_scroll = 0;
            return;
        };
        let height = cairn_log_inner_height(self.term_rows) as usize;
        let max = logs.lines.len().saturating_sub(height);
        self.log_scroll = self.log_scroll.min(max);
    }

    // --- the Strix detail modal -----------------------------------------------------

    pub fn detail(&self) -> Option<&ProgressDetail> {
        self.detail.as_ref()
    }

    pub fn detail_error(&self) -> Option<&str> {
        self.detail_error.as_deref()
    }

    pub fn detail_pane(&self) -> DetailPane {
        self.detail_pane
    }

    pub fn detail_scroll(&self) -> usize {
        self.detail_scroll
    }

    pub fn detail_findings_scroll(&self) -> usize {
        self.detail_findings_scroll
    }

    pub fn detail_follow(&self) -> bool {
        self.detail_follow
    }

    pub fn detail_new(&self) -> usize {
        self.detail_new
    }

    /// The snapshot run the modal is showing, matched by workdir and run. The detail
    /// payload does not carry `live`/`paused`/`pid`, so state is read from the same row the
    /// list renders and a live scan can never read `stale` in the pane.
    pub fn detail_run(&self) -> Option<&RunProgress> {
        let (workdir, run) = self.detail_target.as_ref()?;
        self.runs()
            .iter()
            .find(|r| &r.workdir == workdir && &r.run == run)
    }

    pub fn detail_state(&self) -> String {
        if let Some(run) = self.detail_run() {
            return run.state().to_string();
        }
        match self.detail.as_ref().and_then(|d| d.status.as_deref()) {
            Some("running" | "in_progress") => "stale".to_string(),
            Some(status) => status.to_string(),
            None => "unknown".to_string(),
        }
    }

    pub fn detail_pid(&self) -> Option<i64> {
        self.detail_run().and_then(|run| run.pid)
    }

    pub fn detail_live(&self) -> bool {
        self.detail_run().is_some_and(|run| run.live || run.paused)
    }

    pub fn detail_paused(&self) -> bool {
        self.detail_run().is_some_and(|run| run.paused)
    }

    /// Whether the run has no `.state/agents.db` yet: the modal says so instead of drawing
    /// an empty stream.
    pub fn detail_no_db(&self) -> bool {
        self.detail.is_some() && !self.detail_has_db
    }

    /// The panes' inner text width. The modal keeps the dashboard's left list, as the Ink
    /// verbose view does, so the body is `cols - LEFT_WIDTH`; each pane border eats two more.
    pub fn detail_inner_width(&self) -> usize {
        (self.term_cols.saturating_sub(crate::ui::LEFT_WIDTH + 2)).max(20) as usize
    }

    pub fn detail_header_rows(&self) -> u16 {
        if self.detail.is_some() {
            3
        } else {
            1
        }
    }

    /// The rows above the panes for a failed fetch and a missing `agents.db`: the error
    /// keeps the last good panes below it, so both can show at once.
    pub fn detail_banner_rows(&self) -> u16 {
        (self.detail_error.is_some() as u16) + (self.detail_no_db() as u16)
    }

    /// The FINDINGS and STREAM inner heights, borders removed. One function so the scroll
    /// keys, the follow math and the drawing all agree.
    pub fn detail_pane_heights(&self) -> (u16, u16) {
        let body = self.term_rows.saturating_sub(2);
        let panes = body.saturating_sub(self.detail_header_rows() + self.detail_banner_rows());
        let (upper, lower) = detail_pane_split(panes);
        (
            upper.saturating_sub(2).max(1),
            lower.saturating_sub(2).max(1),
        )
    }

    pub fn detail_findings_len(&self) -> usize {
        self.detail.as_ref().map_or(0, |detail| {
            crate::strix::build_findings(detail, self.detail_inner_width()).len()
        })
    }

    pub fn detail_stream_len(&self) -> usize {
        self.detail.as_ref().map_or(0, |detail| {
            crate::strix::build_stream(detail, self.detail_inner_width()).len()
        })
    }

    pub fn detail_findings_max_scroll(&self) -> usize {
        self.detail_findings_len()
            .saturating_sub(self.detail_pane_heights().0 as usize)
    }

    pub fn detail_max_scroll(&self) -> usize {
        self.detail_stream_len()
            .saturating_sub(self.detail_pane_heights().1 as usize)
    }

    /// Open the Strix modal on the selected run: reset the scroll and follow state and
    /// fetch at once, so a reopen starts at the tail of a fresh payload, not an old offset.
    pub fn open_detail(&mut self) {
        let Some((workdir, run)) = self
            .selected_run()
            .map(|run| (run.workdir.clone(), run.run.clone()))
        else {
            return;
        };
        self.page = Page::Detail;
        self.detail = None;
        self.detail_error = None;
        self.detail_has_db = true;
        self.detail_pane = DetailPane::Stream;
        self.detail_scroll = 0;
        self.detail_findings_scroll = 0;
        self.detail_follow = true;
        self.detail_new = 0;
        self.detail_prev_len = 0;
        self.detail_prev_max = 0;
        self.detail_baseline_id = 0;
        self.detail_reset = true;
        if workdir.is_empty() {
            self.detail_target = None;
            self.detail_error = Some("this run has no directory on disk".to_string());
            return;
        }
        self.detail_target = Some((workdir, run));
        self.next_detail = Instant::now();
        self.start_detail_if_due(Instant::now());
    }

    pub fn close_detail(&mut self) {
        self.page = Page::Dashboard;
        self.detail_rx = None;
    }

    /// Start a detail fetch when one is due and none is in flight. Runs on a worker thread
    /// so a slow CLI can never freeze the keys.
    pub fn start_detail_if_due(&mut self, now: Instant) {
        if self.page != Page::Detail || self.detail_rx.is_some() || now < self.next_detail {
            return;
        }
        let Some((workdir, run)) = self.detail_target.clone() else {
            return;
        };
        self.next_detail = now + self.interval;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(data::fetch_progress_detail(&workdir, &run));
        });
        self.detail_rx = Some(rx);
    }

    /// Refetch because the user pressed `r`, without waiting for the interval.
    pub fn refetch_detail(&mut self) {
        self.next_detail = Instant::now();
        self.start_detail_if_due(Instant::now());
    }

    /// Collect a finished detail fetch. Returns whether anything changed on screen.
    pub fn pump_detail(&mut self, now: Instant) -> bool {
        if self.page != Page::Detail {
            self.detail_rx = None;
            return false;
        }
        let Some(rx) = &self.detail_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(result) => {
                self.detail_rx = None;
                self.apply_detail(result);
                self.next_detail = now + self.interval;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.detail_rx = None;
                self.detail_error = Some("the Strix detail fetch stopped unexpectedly".to_string());
                self.next_detail = now + self.interval;
                true
            }
        }
    }

    fn apply_detail(&mut self, result: Result<ProgressDetail, DataError>) {
        match result {
            Ok(detail) => {
                self.detail_error = None;
                self.detail_has_db = std::path::Path::new(&detail.dir)
                    .join(".state")
                    .join("agents.db")
                    .is_file();
                self.apply_detail_tail(detail);
            }
            Err(error) => {
                // Keep the last good payload: the panes stay useful while the CLI is down.
                self.detail_error = Some(error.to_string());
                self.clamp_detail_scroll();
            }
        }
    }

    /// Follow-the-tail, the same intent the Ink stream pane has: stay pinned to the newest
    /// entry unless the reader scrolled up, then hold still and count what arrived. A new
    /// message is counted by id, so the sliding 200-message window cannot inflate the count.
    fn apply_detail_tail(&mut self, detail: ProgressDetail) {
        let stream_len = crate::strix::build_stream(&detail, self.detail_inner_width()).len();
        let newest_id = detail.newest_message_id();
        // Assign first: `detail_pane_heights` reads whether a payload is present (a header
        // is three rows once it is), so measuring before the assignment would size the
        // stream one row too tall and land the first tail one line above the bottom.
        self.detail = Some(detail);
        let stream_height = self.detail_pane_heights().1 as usize;
        let new_max = stream_len.saturating_sub(stream_height);

        if self.detail_reset {
            self.detail_reset = false;
            self.detail_prev_len = stream_len;
            self.detail_prev_max = new_max;
            self.detail_baseline_id = newest_id;
            self.detail_scroll = new_max;
            self.detail_follow = true;
            self.detail_new = 0;
            self.clamp_detail_scroll();
            return;
        }

        let was_at_bottom = self.detail_scroll >= self.detail_prev_max;
        let grew = stream_len > self.detail_prev_len;
        let shift = stream_len as i64 - self.detail_prev_len as i64;
        self.detail_prev_len = stream_len;
        self.detail_prev_max = new_max;

        // Follow only while the stream is focused. Tabbing to findings freezes it: a new
        // arrival counts as paused rather than moving either pane.
        if self.detail_follow && was_at_bottom && (self.detail_pane == DetailPane::Stream || !grew)
        {
            if self.detail_pane == DetailPane::Stream {
                self.detail_scroll = new_max;
            }
            self.detail_new = 0;
            if grew {
                self.detail_baseline_id = newest_id;
            }
            self.clamp_detail_scroll();
            return;
        }

        if self.detail_follow {
            self.detail_follow = false;
        }
        if self.detail_baseline_id == 0 {
            self.detail_baseline_id = newest_id;
        }
        self.detail_new = self
            .detail
            .as_ref()
            .map(|detail| {
                detail
                    .messages
                    .iter()
                    .filter(|message| message.id > self.detail_baseline_id)
                    .count()
            })
            .unwrap_or(0);
        // When the window slides, lines leave the top: move the paused offset by the same
        // amount so the messages on screen do not jump under the reader.
        let target = (self.detail_scroll as i64 + shift.min(0))
            .max(0)
            .min(new_max as i64) as usize;
        if target != self.detail_scroll {
            self.detail_scroll = target;
        }
        self.clamp_detail_scroll();
    }

    fn clamp_detail_scroll(&mut self) {
        let stream_max = self.detail_max_scroll();
        self.detail_scroll = self.detail_scroll.min(stream_max);
        let findings_max = self.detail_findings_max_scroll();
        self.detail_findings_scroll = self.detail_findings_scroll.min(findings_max);
    }

    /// Scrolling down to the bottom resumes the tail; leaving it pauses.
    fn set_detail_follow_at_bottom(&mut self, max: usize) {
        if self.detail_scroll >= max {
            self.detail_follow = true;
            self.detail_new = 0;
        } else {
            self.detail_follow = false;
            self.detail_baseline_id = self.detail.as_ref().map_or(0, |d| d.newest_message_id());
        }
    }

    // --- input ----------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return;
        }
        // `q` quits from every page except the form, where it is text for the focused field.
        if key.code == KeyCode::Char('q') && self.page != Page::Form {
            self.should_quit = true;
            return;
        }
        match self.page {
            Page::Cairn => self.on_key_cairn(key),
            Page::Detail => self.on_key_detail(key),
            Page::Form => self.on_key_form(key),
            Page::Dashboard => self.on_key_dashboard(key),
        }
    }

    /// The form's keys, mirroring the Ink `Form` handler: arrows/tab move focus, arrows and
    /// space toggle the flow and mode, enter submits, esc cancels, and a focused text field
    /// takes typed characters and backspace. No dashboard key is reachable from here.
    fn on_key_form(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.close_form();
                return;
            }
            KeyCode::Enter => {
                self.submit_form();
                return;
            }
            KeyCode::Up => {
                self.form.row = self.form.row.saturating_sub(1);
                return;
            }
            KeyCode::Down => {
                self.form.row = (self.form.row + 1).min(LAST_ROW);
                return;
            }
            KeyCode::Tab => {
                self.form.row = (self.form.row + 1) % (LAST_ROW + 1);
                return;
            }
            KeyCode::BackTab => {
                self.form.row = (self.form.row + LAST_ROW) % (LAST_ROW + 1);
                return;
            }
            KeyCode::Left => {
                self.form_toggle(false);
                return;
            }
            KeyCode::Right => {
                self.form_toggle(true);
                return;
            }
            _ => {}
        }

        match self.form.row {
            0 => {
                if key.code == KeyCode::Char(' ') {
                    self.form.toggle_flow();
                }
            }
            4 => {
                if key.code == KeyCode::Char(' ') {
                    self.form.cycle_mode(true);
                }
            }
            1..=3 => {
                if matches!(key.code, KeyCode::Backspace | KeyCode::Delete) {
                    self.form.backspace();
                } else if let KeyCode::Char(text) = key.code {
                    // A plain character edits the field; a control/alt chord is not text.
                    let chord = key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    );
                    if !chord {
                        self.form.append(&text.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    /// `←`/`→` on row 0 toggles the flow; on row 4 they step the mode; elsewhere they do
    /// nothing, as the Ink handler has it.
    fn form_toggle(&mut self, forward: bool) {
        match self.form.row {
            0 => self.form.toggle_flow(),
            4 => self.form.cycle_mode(forward),
            _ => {}
        }
    }

    /// Open the form blank, focused on `target` (row 1), exactly as the Ink `n` handler
    /// does. The dashboard keys are unreachable until it closes.
    fn open_form(&mut self) {
        self.form = FormState::blank();
        self.form.row = 1;
        self.page = Page::Form;
    }

    /// `esc`: back to the dashboard unchanged. The form state is discarded on the next open.
    fn close_form(&mut self) {
        self.form.error = None;
        self.page = Page::Dashboard;
    }

    /// Validate, then hand the argv to a worker so a slow CLI cannot freeze the keys. The
    /// dashboard reports the outcome through `message` and refetches the run list.
    fn submit_form(&mut self) {
        if !self.form.check() {
            // The error line is shown in the form; nothing is launched.
            return;
        }
        let fields = self.form.engagement();
        self.page = Page::Dashboard;
        self.message = Some(Message::info(match fields.flow {
            crate::form::Flow::Scan => "starting scan…",
            crate::form::Flow::Engage => "starting engagement…",
        }));
        let flow = fields.flow;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let outcome = match flow {
                crate::form::Flow::Scan => match data::start_scan(&fields) {
                    Ok(line) => SubmitOutcome::Started(line),
                    Err(error) => SubmitOutcome::Failed(error.to_string()),
                },
                crate::form::Flow::Engage => match data::start_engage(&fields) {
                    Ok(log) => SubmitOutcome::Started(format!(
                        "engagement started (full flow); log: {log}"
                    )),
                    Err(error) => SubmitOutcome::Failed(error.to_string()),
                },
            };
            let _ = tx.send(outcome);
        });
        self.submit_rx = Some(rx);
    }

    /// Collect a finished submit. Returns whether anything changed on screen. A success or
    /// failure both refresh the list, so a newly started run shows up on the next frame.
    pub fn pump_submit(&mut self) -> bool {
        let Some(rx) = &self.submit_rx else {
            return false;
        };
        match rx.try_recv() {
            Ok(outcome) => {
                self.submit_rx = None;
                self.message = Some(match outcome {
                    SubmitOutcome::Started(text) => Message::ok(text),
                    SubmitOutcome::Failed(text) => Message::err(text),
                });
                self.next_poll = Instant::now();
                self.start_poll_if_due(Instant::now());
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.submit_rx = None;
                self.message = Some(Message::err("the submit stopped unexpectedly"));
                true
            }
        }
    }

    /// The Cairn page's keys, mirroring the Ink modal: esc/enter close, tab switches
    /// panes, arrows and PgUp/PgDn scroll the focused one, g/G jump, r refetches.
    fn on_key_cairn(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            self.close_cairn();
            return;
        }
        if key.code == KeyCode::Tab {
            self.cairn_pane = match self.cairn_pane {
                CairnPane::Graph => CairnPane::Logs,
                CairnPane::Logs => CairnPane::Graph,
            };
            return;
        }
        if key.code == KeyCode::Char('r') {
            self.refetch_cairn();
            return;
        }

        let graph_height = cairn_graph_canvas_height(self.term_rows);
        let log_height = cairn_log_inner_height(self.term_rows) as usize;
        let graph_max = self.graph_max_scroll();
        let log_max = self
            .logs
            .as_ref()
            .map_or(0, |logs| logs.lines.len().saturating_sub(log_height));

        let step = match key.code {
            KeyCode::Up | KeyCode::Char('k') => -1,
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::PageUp => -(graph_height as isize),
            KeyCode::PageDown => graph_height as isize,
            _ => 0,
        };
        if step != 0 {
            match self.cairn_pane {
                CairnPane::Graph => {
                    let next = (self.graph_scroll as isize + step).clamp(0, graph_max as isize);
                    self.graph_scroll = next as u16;
                }
                CairnPane::Logs => {
                    let next = (self.log_scroll as isize + step).clamp(0, log_max as isize);
                    self.log_scroll = next as usize;
                    self.set_log_follow_at_bottom(log_max);
                }
            }
            return;
        }

        let top = matches!(key.code, KeyCode::Home | KeyCode::Char('g'));
        let end = matches!(key.code, KeyCode::End | KeyCode::Char('G'));
        if !top && !end {
            return;
        }
        match self.cairn_pane {
            CairnPane::Graph => {
                self.graph_scroll = if top { 0 } else { graph_max };
            }
            CairnPane::Logs => {
                if top {
                    self.log_follow = false;
                    self.log_new = 0;
                    self.log_scroll = 0;
                } else {
                    self.log_follow = true;
                    self.log_new = 0;
                    self.log_scroll = log_max;
                }
            }
        }
    }

    /// Scrolling down to the bottom resumes the tail; leaving it pauses and clears the
    /// "new" badge only when the reader is back at the bottom.
    fn set_log_follow_at_bottom(&mut self, log_max: usize) {
        if self.log_scroll >= log_max {
            self.log_follow = true;
            self.log_new = 0;
        } else {
            self.log_follow = false;
        }
    }

    /// The Strix modal's keys, mirroring the Ink verbose view: esc/enter close, tab
    /// switches panes, arrows and PgUp/PgDn scroll the focused one, g/G jump, r refetches.
    fn on_key_detail(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Esc | KeyCode::Enter) {
            self.close_detail();
            return;
        }
        if key.code == KeyCode::Tab {
            self.detail_pane = match self.detail_pane {
                DetailPane::Findings => DetailPane::Stream,
                DetailPane::Stream => DetailPane::Findings,
            };
            return;
        }
        if key.code == KeyCode::Char('r') {
            self.refetch_detail();
            return;
        }

        let (findings_height, stream_height) = self.detail_pane_heights();
        let stream_max = self.detail_max_scroll();
        let findings_max = self.detail_findings_max_scroll();
        let page = match self.detail_pane {
            DetailPane::Findings => findings_height,
            DetailPane::Stream => stream_height,
        } as isize;

        let step = match key.code {
            KeyCode::Up | KeyCode::Char('k') => -1isize,
            KeyCode::Down | KeyCode::Char('j') => 1,
            KeyCode::PageUp => -page,
            KeyCode::PageDown => page,
            _ => 0,
        };
        if step != 0 {
            match self.detail_pane {
                DetailPane::Findings => {
                    let next = (self.detail_findings_scroll as isize + step)
                        .clamp(0, findings_max as isize);
                    self.detail_findings_scroll = next as usize;
                }
                DetailPane::Stream => {
                    let next = (self.detail_scroll as isize + step).clamp(0, stream_max as isize);
                    self.detail_scroll = next as usize;
                    self.set_detail_follow_at_bottom(stream_max);
                }
            }
            return;
        }

        let top = matches!(key.code, KeyCode::Home | KeyCode::Char('g'));
        let end = matches!(key.code, KeyCode::End | KeyCode::Char('G'));
        if !top && !end {
            return;
        }
        match self.detail_pane {
            DetailPane::Findings => {
                self.detail_findings_scroll = if top { 0 } else { findings_max };
            }
            DetailPane::Stream => {
                if top {
                    self.detail_follow = false;
                    self.detail_new = 0;
                    self.detail_baseline_id =
                        self.detail.as_ref().map_or(0, |d| d.newest_message_id());
                    self.detail_scroll = 0;
                } else {
                    self.detail_follow = true;
                    self.detail_new = 0;
                    self.detail_scroll = stream_max;
                }
            }
        }
    }

    fn on_key_dashboard(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('n') => self.open_form(),
            KeyCode::Up | KeyCode::Char('k') => self.select(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select(1),
            KeyCode::Tab => {
                self.target = match self.target {
                    Target::Run => Target::Cairn,
                    Target::Cairn => Target::Run,
                };
            }
            KeyCode::Enter => match self.target {
                Target::Cairn => self.open_cairn(),
                Target::Run => self.open_detail(),
            },
            KeyCode::Char('r') => {
                // Refresh now: bypass the wait but still go through the worker thread.
                self.next_poll = Instant::now();
                self.start_poll_if_due(Instant::now());
            }
            _ => {}
        }
    }

    fn select(&mut self, delta: isize) {
        let count = self.runs().len();
        if count == 0 {
            return;
        }
        let last = count - 1;
        let next = self.selected.saturating_add_signed(delta).min(last);
        if next != self.selected {
            self.selected = next;
            self.sample_telemetry();
        }
    }

    // --- refresh internals ----------------------------------------------------------

    fn apply(&mut self, result: Result<Snapshot, DataError>) {
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
                let count = self.runs().len();
                self.selected = self.selected.min(count.saturating_sub(1));
            }
            Err(error) => {
                // Keep the last good snapshot: the lists stay useful while the CLI is down.
                self.error = Some(error.to_string());
            }
        }
        self.sample_telemetry();
        self.poll_seq = self.poll_seq.wrapping_add(1);
    }

    /// Read the two processes the header and detail pane care about. File reads only:
    /// the poll stays a single CLI call.
    fn sample_telemetry(&mut self) {
        let mut pids = Vec::new();
        if let Some(pid) = self.dispatcher_pid() {
            pids.push(pid);
        }
        if let Some(pid) = self.selected_run().and_then(|r| r.pid) {
            pids.push(pid);
        }

        for pid in pids {
            let Some(sample) = read_proc(pid) else {
                self.proc_prev.remove(&pid);
                self.proc_cpu.insert(pid, None);
                self.proc_rss.insert(pid, None);
                continue;
            };
            let cpu = self
                .proc_prev
                .get(&pid)
                .and_then(|prev| cpu_percent(sample, *prev));
            self.proc_prev.insert(pid, sample);
            self.proc_cpu.insert(pid, cpu);
            self.proc_rss.insert(pid, Some(sample.rss_kb));
        }

        let dir = self.selected_run().map_or(String::new(), |r| r.dir.clone());
        if dir != self.history_dir {
            self.history_dir = dir;
            self.cpu_history.clear();
        }
        if let Some(pid) = self.selected_run().and_then(|r| r.pid) {
            if let Some(Some(cpu)) = self.proc_cpu.get(&pid) {
                self.cpu_history.push(cpu.round().clamp(0.0, 999.0) as u64);
                if self.cpu_history.len() > HISTORY_LEN {
                    let excess = self.cpu_history.len() - HISTORY_LEN;
                    self.cpu_history.drain(0..excess);
                }
            }
        }
    }
}

fn read_proc(pid: i64) -> Option<ProcSample> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The comm field is parenthesised and may contain spaces or parens, so slice it off
    // before splitting: field 14/15 (utime/stime) are then at 11/12 of the remainder.
    let tail = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let ticks = fields.get(11)?.parse::<i64>().ok()? + fields.get(12)?.parse::<i64>().ok()?;
    let rss_kb = std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                let value = line.strip_prefix("VmRSS:")?;
                value.split_whitespace().next()?.parse::<i64>().ok()
            })
        })
        .unwrap_or(0);
    Some(ProcSample {
        ticks,
        rss_kb,
        at: Instant::now(),
    })
}

fn cpu_percent(now: ProcSample, before: ProcSample) -> Option<f64> {
    let seconds = now.at.duration_since(before.at).as_secs_f64();
    if seconds <= 0.0 {
        return None;
    }
    let delta = (now.ticks - before.ticks) as f64;
    Some((delta / CLK_TCK / seconds * 100.0).max(0.0))
}
