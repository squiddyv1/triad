//! Application state and key handling. The drawing code in `ui` reads this and nothing
//! else, so no widget has to know how a snapshot is fetched or a key is routed.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::data::{self, DataError, Project, RunProgress, Snapshot};

/// The spinner cadence: ten frames, roughly one revolution per second. The frame only
/// advances while something is live, so an idle dashboard never redraws on its own.
pub const TICK: Duration = Duration::from_millis(100);
const HISTORY_LEN: usize = 60;
const CLK_TCK: f64 = 100.0; // Linux userspace default
/// A poll result must be noticed promptly, but an idle loop should still sleep. This caps
/// the event wait so a completed background poll is never stuck behind a long blocking read.
const MAX_WAIT: Duration = Duration::from_millis(150);

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

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn cpu_history(&self) -> &[u64] {
        &self.cpu_history
    }

    pub fn proc_cpu(&self, pid: i64) -> Option<f64> {
        self.proc_cpu.get(&pid).copied().flatten()
    }

    pub fn proc_rss(&self, pid: i64) -> Option<i64> {
        self.proc_rss.get(&pid).copied().flatten()
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

    /// Advance the spinner while something is live. Returns whether to redraw.
    pub fn tick(&mut self, now: Instant) -> bool {
        if !self.animating() || now < self.last_tick + TICK {
            return false;
        }
        self.spinner = self.spinner.wrapping_add(1);
        self.last_tick = now;
        true
    }

    /// How long the event wait may block before timers want attention again.
    pub fn wait_hint(&self, now: Instant) -> Duration {
        let mut wait = self.next_poll.saturating_duration_since(now);
        if self.animating() {
            let until_tick = (self.last_tick + TICK).saturating_duration_since(now);
            wait = wait.min(until_tick);
        }
        wait.min(MAX_WAIT)
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    // --- input ----------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Up | KeyCode::Char('k') => self.select(-1),
            KeyCode::Down | KeyCode::Char('j') => self.select(1),
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
