//! The CLI bridge: one snapshot per poll, straight from `triad runs --json`.
//!
//! State is read from the CLI rather than re-derived here, so the dashboard and the
//! command line can never disagree about what is running. Commands are spawned with an
//! argv built in process, never through a shell, so no argument can be re-interpreted.
//!
//! `fetch_snapshot` is the only payload this stage needs. Later stages add their own
//! `fetch_*` functions beside it, each one spawning the same interpreter and script.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::io::{self, Read};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::form::{self, NewEngagement};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(20);
const DEFAULT_PYTHON: &str = "python3";
const DEFAULT_SCRIPT: &str = "triad.py";

/// Everything a dashboard needs in one call, mirroring the `Snapshot` type in the Ink app.
/// The DTOs stay faithful to the payload rather than being trimmed to this stage's use,
/// so later stages read the fields the CLI already sends.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub cairn: Cairn,
    #[serde(default)]
    pub dispatcher: Dispatcher,
    #[serde(default)]
    pub runs: Vec<RunProgress>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Cairn {
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub up: bool,
    #[serde(default)]
    pub projects: Vec<Project>,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct Dispatcher {
    #[serde(default)]
    pub pid: Option<i64>,
    #[serde(default)]
    pub alive: bool,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub fact_count: Option<i64>,
    #[serde(default)]
    pub hint_count: Option<i64>,
    #[serde(default)]
    pub intent_count: Option<i64>,
    #[serde(default)]
    pub unclaimed_intent_count: Option<i64>,
    #[serde(default)]
    pub working_intent_count: Option<i64>,
}

impl Project {
    pub fn facts(&self) -> i64 {
        self.fact_count.unwrap_or(0)
    }
    pub fn hints(&self) -> i64 {
        self.hint_count.unwrap_or(0)
    }
    pub fn intents(&self) -> i64 {
        self.intent_count.unwrap_or(0)
    }
    pub fn unclaimed(&self) -> i64 {
        self.unclaimed_intent_count.unwrap_or(0)
    }
    pub fn working(&self) -> i64 {
        self.working_intent_count.unwrap_or(0)
    }
    pub fn open(&self) -> i64 {
        self.unclaimed() + self.working()
    }
}

/// One Strix run, mirroring `RunProgress` in the Ink app. Nullable fields stay optional
/// so a half-written `run.json` renders as blank rather than failing the whole snapshot.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct RunProgress {
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub workdir: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub start_time: Option<String>,
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub turns: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub findings: i64,
    #[serde(default)]
    pub findings_by_severity: Option<BTreeMap<String, i64>>,
    #[serde(default)]
    pub coverage_gaps: i64,
    #[serde(default)]
    pub notes: i64,
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub pid: Option<i64>,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub project_fed_at: Option<String>,
    #[serde(default)]
    pub agents: Agents,
    #[serde(default)]
    pub todos: Todos,
    #[serde(default)]
    pub todos_detail: Option<Vec<TodoDetail>>,
    #[serde(default)]
    pub usage: Usage,
}

impl RunProgress {
    /// The state the dashboard shows. A run.json that still says "running" for a process
    /// that is gone reads as `stale`, never `running`: that is the one state that would
    /// otherwise claim work is happening when nothing is.
    pub fn state(&self) -> &str {
        if self.paused {
            "paused"
        } else if self.live {
            "running"
        } else if matches!(self.status.as_deref(), Some("running" | "in_progress")) {
            "stale"
        } else {
            self.status.as_deref().unwrap_or("unknown")
        }
    }

    /// Only a scan that is actually going animates; a paused one is live on disk but idle.
    pub fn is_animating(&self) -> bool {
        self.live && !self.paused
    }

    pub fn severity(&self, name: &str) -> i64 {
        self.findings_by_severity
            .as_ref()
            .and_then(|m| m.get(name))
            .copied()
            .unwrap_or(0)
    }
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Agents {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub completed: i64,
    #[serde(default)]
    pub running: Vec<String>,
    #[serde(default)]
    pub waiting: i64,
    #[serde(default)]
    pub failed: i64,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Todos {
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub done: i64,
    #[serde(default)]
    pub in_progress: i64,
    #[serde(default)]
    pub pending: i64,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct TodoDetail {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub requests: Option<i64>,
    #[serde(default)]
    pub input_tokens: Option<i64>,
    #[serde(default)]
    pub cached_tokens: Option<i64>,
    #[serde(default)]
    pub output_tokens: Option<i64>,
}

/// Why a CLI call did not produce a payload. Every variant carries enough text to show
/// the user in the pane, because the dashboard never exits on a failed poll.
#[derive(Debug)]
pub enum DataError {
    Spawn {
        cmd: String,
        source: io::Error,
    },
    Timeout {
        cmd: String,
        secs: u64,
    },
    Exit {
        cmd: String,
        code: Option<i32>,
        stderr: String,
    },
    Read(io::Error),
    Json(serde_json::Error),
    Io(io::Error),
}

impl fmt::Display for DataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DataError::Spawn { cmd, source } => write!(f, "{cmd} could not be started: {source}"),
            DataError::Timeout { cmd, secs } => write!(f, "{cmd} timed out after {secs}s"),
            DataError::Exit { cmd, code, stderr } => {
                if stderr.is_empty() {
                    write!(
                        f,
                        "{cmd} exited {}",
                        code.map_or("?".into(), |c| c.to_string())
                    )
                } else {
                    write!(f, "{stderr}")
                }
            }
            DataError::Read(source) => write!(f, "reading the CLI's output failed: {source}"),
            DataError::Json(source) => write!(f, "the CLI returned invalid JSON: {source}"),
            DataError::Io(source) => {
                write!(f, "preparing the engagement directory failed: {source}")
            }
        }
    }
}

impl std::error::Error for DataError {}

/// The interpreter and script the launcher told us to use. Same env vars and defaults the
/// Ink app reads, so one launcher can start either binary.
pub fn cli() -> (String, String) {
    let python = env::var("TRIAD_PYTHON").unwrap_or_else(|_| DEFAULT_PYTHON.to_string());
    let script = env::var("TRIAD_PY").unwrap_or_else(|_| DEFAULT_SCRIPT.to_string());
    (python, script)
}

/// Run the CLI and return its stdout. A timeout kills the child instead of hanging the
/// dashboard; stdout and stderr are drained on their own threads so a large payload can
/// never deadlock on a full pipe.
fn run_command(cmd: &str, args: &[String], timeout: Duration) -> Result<String, DataError> {
    let display = format_command(cmd, args);
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|source| DataError::Spawn {
            cmd: display.clone(),
            source,
        })?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| DataError::Read(io::Error::other("no stdout pipe")))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| DataError::Read(io::Error::other("no stderr pipe")))?;
    let out_handle = thread::spawn(move || read_all(stdout));
    let err_handle = thread::spawn(move || read_all(stderr));

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = out_handle.join();
                    let _ = err_handle.join();
                    return Err(DataError::Timeout {
                        cmd: display,
                        secs: timeout.as_secs(),
                    });
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(source) => {
                return Err(DataError::Spawn {
                    cmd: display,
                    source,
                })
            }
        }
    };

    let out = join_read(out_handle)?;
    let err = join_read(err_handle)?;
    if status.success() {
        Ok(out)
    } else {
        Err(DataError::Exit {
            cmd: display,
            code: status.code(),
            stderr: err.trim().to_string(),
        })
    }
}

fn read_all<R: Read>(mut reader: R) -> io::Result<String> {
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

fn join_read(handle: thread::JoinHandle<io::Result<String>>) -> Result<String, DataError> {
    match handle.join() {
        Ok(result) => result.map_err(DataError::Read),
        Err(_) => Err(DataError::Read(io::Error::other(
            "CLI reader thread panicked",
        ))),
    }
}

fn format_command(cmd: &str, args: &[String]) -> String {
    std::iter::once(cmd)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `triad runs --json`, the single poll the dashboard runs.
pub fn fetch_snapshot() -> Result<Snapshot, DataError> {
    let (python, script) = cli();
    let args = vec![script, "runs".to_string(), "--json".to_string()];
    let out = run_command(&python, &args, DEFAULT_TIMEOUT)?;
    serde_json::from_str(&out).map_err(DataError::Json)
}

// --- starting a new engagement ------------------------------------------------------

/// `triad scan --target ... --workdir ... --mode ...`, mirroring the Ink `startScan`.
/// `triad scan` returns as soon as Strix is launched, so this resolves with its
/// `strix started pid=...` line once the directory and pid file are on disk. The engagement
/// directory is created first, as the Ink path does, so the CLI has somewhere to write.
pub fn start_scan(fields: &NewEngagement) -> Result<String, DataError> {
    let dir = form::engagement_workdir(fields);
    std::fs::create_dir_all(&dir).map_err(DataError::Io)?;
    let (python, script) = cli();
    let mut args = vec![script];
    args.extend(form::scan_args(fields));
    let out = run_command(&python, &args, DEFAULT_TIMEOUT)?;
    let line = out
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("strix started"));
    Ok(line.map(str::to_string).unwrap_or_else(|| {
        out.trim()
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .map(|line| line.trim().to_string())
            .unwrap_or_else(|| "scan launched".to_string())
    }))
}

/// `triad engage ...`, spawned detached with its output appended to `triad-engage.log`, so
/// the full flow (which runs for the whole engagement) outlives the dashboard. Mirrors the
/// Ink `startEngage`; the returned path is the log the dashboard reports. A new process
/// group detaches it from the dashboard's terminal, so closing the dashboard does not hang
/// the engagement up.
pub fn start_engage(fields: &NewEngagement) -> Result<String, DataError> {
    let dir = form::engagement_workdir(fields);
    std::fs::create_dir_all(&dir).map_err(DataError::Io)?;
    let log = dir.join("triad-engage.log");
    let out_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(DataError::Io)?;
    let err_file = out_file.try_clone().map_err(DataError::Io)?;

    let (python, script) = cli();
    let mut args = vec![script];
    args.extend(form::engage_args(fields));
    let mut command = Command::new(&python);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out_file))
        .stderr(Stdio::from(err_file));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command.spawn().map_err(|source| DataError::Spawn {
        cmd: format_command(&python, &args),
        source,
    })?;
    // The child is deliberately not waited on or reaped: it is detached, and the dashboard
    // must not hold its lifetime.
    Ok(log.to_string_lossy().into_owned())
}

// --- the Cairn page's two payloads ------------------------------------------------

/// `graph --json`: the layout-ready project graph, mirroring `ProjectGraph` in the Ink app.
/// `to` is null on an intent that has not concluded; that is the frontier the canvas draws
/// as a stub reaching forward rather than a line to nowhere.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ProjectGraph {
    #[serde(default)]
    pub project: GraphProject,
    #[serde(default)]
    pub nodes: Vec<GraphNode>,
    #[serde(default)]
    pub edges: Vec<GraphEdge>,
    #[serde(default)]
    pub counts: GraphCounts,
    #[serde(default)]
    pub path: Vec<PathStep>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct GraphProject {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub status: String,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct GraphNode {
    #[serde(default)]
    pub id: String,
    /// `origin`, `goal`, `fact` or `hint`.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub hop: i64,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct GraphEdge {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub from: Vec<String>,
    #[serde(default)]
    pub to: Option<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub worker: Option<String>,
    #[serde(default)]
    pub label: String,
}

impl GraphEdge {
    pub fn is_concluded(&self) -> bool {
        self.status == "concluded"
    }
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct GraphCounts {
    #[serde(default)]
    pub facts: i64,
    #[serde(default)]
    pub hints: i64,
    #[serde(default)]
    pub intents: i64,
    #[serde(default)]
    pub open: i64,
    #[serde(default)]
    pub concluded: i64,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct PathStep {
    #[serde(default)]
    pub fact: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub via: Option<String>,
    #[serde(default)]
    pub worker: Option<String>,
}

/// `cairn-logs --json`: the tail and where it came from. `source: "none"` is a normal
/// answer, not an error: the pane explains it rather than showing an empty box.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct CairnLogs {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default)]
    pub lines: Vec<String>,
}

// --- the Strix modal's verbose payload --------------------------------------------

/// `progress --verbose --json`: the summary `RunProgress` already carries, plus the detail
/// a human wants -- the findings by severity with their titles, the coverage gaps, and the
/// agent stream. Mirrors `ProgressDetail` in the Ink app; unknown keys are ignored, so the
/// CLI can add more without breaking this view.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct ProgressDetail {
    #[serde(default)]
    pub run: String,
    #[serde(default)]
    pub dir: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub start_time: Option<String>,
    #[serde(default)]
    pub end_time: Option<String>,
    #[serde(default)]
    pub turns: Option<i64>,
    #[serde(default)]
    pub cost_usd: Option<f64>,
    #[serde(default)]
    pub findings: i64,
    #[serde(default)]
    pub findings_by_severity: Option<BTreeMap<String, i64>>,
    #[serde(default)]
    pub coverage_gaps: i64,
    #[serde(default)]
    pub notes: i64,
    #[serde(default)]
    pub agents: Agents,
    #[serde(default)]
    pub todos: Todos,
    #[serde(default)]
    pub todos_detail: Option<Vec<TodoDetail>>,
    #[serde(default)]
    pub usage: Usage,
    #[serde(default)]
    pub agents_detail: Vec<AgentDetail>,
    #[serde(default)]
    pub findings_detail: Vec<FindingDetail>,
    #[serde(default)]
    pub coverage: Option<Coverage>,
    #[serde(default)]
    pub messages: Vec<AgentMessage>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct AgentDetail {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub pending: i64,
}

#[derive(Debug, Default, Clone, Deserialize)]
pub struct FindingDetail {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub severity: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct NoteDetail {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub agent_name: Option<String>,
}

/// Coverage is split across files: Strix's own summary and the gaps list. The summary's
/// shape belongs to Strix, so it is read field by field and unknown keys are ignored.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct Coverage {
    #[serde(default)]
    pub summary: Option<CoverageSummary>,
    #[serde(default)]
    pub gaps: Vec<serde_json::Value>,
}

#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct CoverageSummary {
    #[serde(default)]
    pub surfaces_reviewed: Option<i64>,
    #[serde(default)]
    pub findings_filed: Option<i64>,
    #[serde(default)]
    pub gaps: Option<i64>,
}

impl Coverage {
    /// The gap count the pane heading shows: the rendered list when there is one, else the
    /// summary's own number, so a heading can never claim more gaps than it lists.
    pub fn gap_count(&self, fallback: i64) -> i64 {
        if !self.gaps.is_empty() {
            self.gaps.len() as i64
        } else {
            self.summary
                .as_ref()
                .and_then(|summary| summary.gaps)
                .unwrap_or(fallback)
        }
    }
}

/// One row of the agent stream, from `.state/agents.db`. `kind` is `message`,
/// `reasoning`, `function_call` or `function_call_output`; the CLI has already clamped
/// the text and flagged whether it truncated.
#[allow(dead_code)]
#[derive(Debug, Default, Clone, Deserialize)]
pub struct AgentMessage {
    #[serde(default)]
    pub id: i64,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub agent_name: String,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub truncated: bool,
}

impl ProgressDetail {
    /// The findings list the pane renders, or the summary count when no list came back.
    pub fn findings_count(&self) -> usize {
        if self.findings_detail.is_empty() {
            self.findings.max(0) as usize
        } else {
            self.findings_detail.len()
        }
    }

    /// The newest message id, the anchor follow-the-tail counts arrivals against. A `0`
    /// means there is no message window yet.
    pub fn newest_message_id(&self) -> i64 {
        self.messages.last().map_or(0, |message| message.id)
    }

    /// The live activity line the header shows: the first running agent, else the first
    /// in-progress todo, else nothing.
    pub fn activity(&self) -> Option<&str> {
        if let Some(name) = self.agents.running.first() {
            return Some(name.as_str());
        }
        self.todos_detail
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .find(|todo| todo.status.as_deref() == Some("in_progress"))
            .and_then(|todo| todo.title.as_deref().or(Some(todo.id.as_str())))
    }
}

/// Both Cairn page payloads, fetched together on one worker thread. `graph` is `None`
/// when the run has no linked project: there is nothing to ask the CLI for, and the page
/// says so rather than inventing a graph.
pub struct CairnData {
    pub graph: Option<Result<ProjectGraph, DataError>>,
    pub logs: Result<CairnLogs, DataError>,
}

/// Fetch the graph and the log tail for the Cairn page. The graph is only requested when
/// a project is linked; the logs are useful on their own, so they are always requested.
pub fn fetch_cairn(linked: Option<String>) -> CairnData {
    let graph = linked.map(|project| fetch_graph(&project));
    CairnData {
        graph,
        logs: fetch_cairn_logs(200),
    }
}

/// `triad graph --project <id> --json`.
pub fn fetch_graph(project: &str) -> Result<ProjectGraph, DataError> {
    let (python, script) = cli();
    let args = vec![
        script,
        "graph".to_string(),
        "--project".to_string(),
        project.to_string(),
        "--json".to_string(),
    ];
    let out = run_command(&python, &args, DEFAULT_TIMEOUT)?;
    serde_json::from_str(&out).map_err(DataError::Json)
}

/// `triad progress --verbose --json --messages 200 --log-lines 0`, the Strix modal's
/// payload. The log tail is deliberately not requested: the modal renders the agent
/// stream instead, so those bytes are never paid for.
pub fn fetch_progress_detail(workdir: &str, run: &str) -> Result<ProgressDetail, DataError> {
    let (python, script) = cli();
    let args = vec![
        script,
        "progress".to_string(),
        "--verbose".to_string(),
        "--json".to_string(),
        "--workdir".to_string(),
        workdir.to_string(),
        "--run".to_string(),
        run.to_string(),
        "--messages".to_string(),
        "200".to_string(),
        "--log-lines".to_string(),
        "0".to_string(),
    ];
    let out = run_command(&python, &args, DEFAULT_TIMEOUT)?;
    serde_json::from_str(&out).map_err(DataError::Json)
}

/// `triad cairn-logs --json --lines <n>`.
pub fn fetch_cairn_logs(lines: usize) -> Result<CairnLogs, DataError> {
    let (python, script) = cli();
    let args = vec![
        script,
        "cairn-logs".to_string(),
        "--json".to_string(),
        "--lines".to_string(),
        lines.to_string(),
    ];
    let out = run_command(&python, &args, DEFAULT_TIMEOUT)?;
    serde_json::from_str(&out).map_err(DataError::Json)
}

/// Human sizes for tokens and memory: "18M", "279k", "1.4G".
pub fn human(n: Option<i64>) -> String {
    let Some(n) = n else {
        return "-".to_string();
    };
    for (unit, size) in [("G", 1_000_000_000_i64), ("M", 1_000_000), ("k", 1_000)] {
        if n >= size {
            let value = n as f64 / size as f64;
            let text = format!("{value:.1}");
            return match text.strip_suffix(".0") {
                Some(trimmed) => format!("{trimmed}{unit}"),
                None => format!("{text}{unit}"),
            };
        }
    }
    n.to_string()
}

/// Kilobyte sizes (as /proc reports them) through the same humaniser.
pub fn human_kb(kb: Option<i64>) -> String {
    match kb {
        Some(kb) if kb > 0 => human(Some(kb.saturating_mul(1024))),
        _ => "-".to_string(),
    }
}

/// How long a run has been going, or took: "12m", "1h03m". Mirrors the CLI's `_elapsed`.
pub fn elapsed(start: Option<&str>, end: Option<&str>) -> String {
    let Some(start) = start else {
        return "-".to_string();
    };
    let Some(began) = parse_iso8601(start) else {
        return "-".to_string();
    };
    let stopped = match end {
        Some(end) => parse_iso8601(end).unwrap_or_else(now_epoch),
        None => now_epoch(),
    };
    let minutes = ((stopped - began).max(0)) / 60;
    if minutes >= 60 {
        format!("{}h{:02}m", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}m")
    }
}

/// "3s ago", "12 min ago", "2h05m ago", "4d ago".
pub fn relative_age(iso: Option<&str>) -> String {
    let Some(iso) = iso else {
        return "-".to_string();
    };
    let Some(at) = parse_iso8601(iso) else {
        return "-".to_string();
    };
    let seconds = (now_epoch() - at).max(0);
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3600 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 86_400 {
        let minutes = seconds / 60;
        format!("{}h{:02}m ago", minutes / 60, minutes % 60)
    } else {
        format!("{}d ago", seconds / 86_400)
    }
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// Parse the ISO 8601 timestamps Strix writes, e.g. `2026-10-05T11:54:50.830312+00:00`.
/// A timestamp with no offset is treated as UTC; anything unparseable yields `None` and
/// the caller renders `-` rather than guessing a duration.
fn parse_iso8601(text: &str) -> Option<i64> {
    let text = text.trim();
    let (date, rest) = text.split_once(['T', ' '])?;

    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    if date.next().is_some() {
        return None;
    }

    let (time, offset) = split_offset(rest);
    let mut time = time.split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: i64 = time.next().unwrap_or("0").split('.').next()?.parse().ok()?;

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second - offset)
}

/// Split a time away from its offset and return the offset in seconds east of UTC.
/// `Z`, `+HH:MM` and `-HH:MM` are understood; no offset at all means UTC.
fn split_offset(rest: &str) -> (&str, i64) {
    if let Some(time) = rest.strip_suffix(['Z', 'z']) {
        return (time, 0);
    }
    match rest.rfind(['+', '-']) {
        Some(index) => {
            let (time, offset) = rest.split_at(index);
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let mut parts = offset[1..].split(':');
            let hours: i64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let minutes: i64 = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            if time.is_empty() {
                (rest, 0)
            } else {
                (time, sign * (hours * 3_600 + minutes * 60))
            }
        }
        None => (rest, 0),
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_offset_timestamps() {
        // 2026-10-05T11:54:50Z
        assert_eq!(
            parse_iso8601("2026-10-05T11:54:50+00:00"),
            Some(1_791_201_290)
        );
        assert_eq!(parse_iso8601("2026-10-05T11:54:50Z"), Some(1_791_201_290));
        assert_eq!(
            parse_iso8601("2026-10-05T13:54:50+02:00"),
            Some(1_791_201_290)
        );
    }

    #[test]
    fn parses_fractional_seconds() {
        assert_eq!(
            parse_iso8601("2026-10-05T11:54:50.830312+00:00"),
            Some(1_791_201_290)
        );
    }

    #[test]
    fn humanises_sizes() {
        assert_eq!(human(Some(18_153_721)), "18.2M");
        assert_eq!(human(Some(278_962)), "279k");
        assert_eq!(human(None), "-");
    }

    #[test]
    fn state_vocabulary() {
        let mut run = RunProgress {
            status: Some("running".into()),
            ..Default::default()
        };
        assert_eq!(run.state(), "stale");
        run.live = true;
        assert_eq!(run.state(), "running");
        run.paused = true;
        assert_eq!(run.state(), "paused");
    }

    #[test]
    fn parses_progress_detail() {
        // A trimmed but faithful slice of `progress --verbose --json`: the summary keys,
        // the extra detail keys, a coverage summary whose shape is Strix's, and the stream.
        let payload = r#"{
            "run": "r1", "dir": "/tmp/r1", "status": "running",
            "start_time": "2026-10-05T11:54:50+00:00", "end_time": null,
            "cost_usd": 0.0, "findings": 2, "coverage_gaps": 1,
            "agents": {"total": 3, "completed": 2, "running": ["Reporter"], "failed": 0},
            "todos": {"total": 4, "done": 3, "in_progress": 1, "pending": 0},
            "usage": {"requests": 9, "input_tokens": 1000, "output_tokens": 20},
            "findings_detail": [{"title": "RCE", "severity": "critical"},
                                {"title": "SQLi", "severity": "high"}],
            "coverage": {"summary": {"surfaces_reviewed": 5, "findings_filed": 2, "gaps": 7},
                         "gaps": [{"message": "unexamined"}]},
            "messages": [
                {"id": 4, "session_id": "s", "agent_name": "Reporter", "role": "assistant",
                 "type": "message", "text": "done", "tool": null,
                 "at": "2026-10-05 12:13:12", "truncated": false},
                {"id": 9, "session_id": "s", "agent_name": "Reporter", "role": "assistant",
                 "type": "function_call", "text": "x=1", "tool": "shell",
                 "at": "2026-10-05 12:13:20", "truncated": true}
            ]
        }"#;
        let detail: ProgressDetail = serde_json::from_str(payload).unwrap();
        assert_eq!(detail.run, "r1");
        assert_eq!(detail.agents.completed, 2);
        assert_eq!(detail.findings_count(), 2);
        assert_eq!(detail.newest_message_id(), 9);
        assert_eq!(detail.activity(), Some("Reporter"));
        assert_eq!(detail.messages[1].kind, "function_call");
        assert_eq!(detail.messages[1].tool.as_deref(), Some("shell"));
        assert!(detail.messages[1].truncated);
        let coverage = detail.coverage.unwrap();
        assert_eq!(coverage.gap_count(99), 1);
        assert_eq!(coverage.summary.unwrap().surfaces_reviewed, Some(5));
    }

    #[test]
    fn coverage_count_falls_back_to_summary() {
        let coverage: Coverage =
            serde_json::from_str(r#"{"summary":{"gaps":7},"gaps":[]}"#).unwrap();
        assert_eq!(coverage.gap_count(99), 7);
        assert_eq!(Coverage::default().gap_count(3), 3);
    }
}
