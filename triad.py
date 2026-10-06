#!/usr/bin/env python3
"""triad: command-line driver for the Strix + Cairn stack.

The normal entry point, and it needs no agent framework: it loads `plugin/` directly.
Hermes, when installed, exposes that same package as tools.

    triad setup     one provider for Strix and the worker, then start the stack
    triad engage    create the project, run the scan, feed the findings into the graph
    triad report    write it up

`triad --help` lists the rest. Local mode is the default, because the containerised
dispatcher needs the amd64-only worker image: `triad up` runs the dispatcher on the host
and `triad down` stops it and the server.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import time
import types
import urllib.error
import urllib.request
import uuid
from datetime import datetime
from pathlib import Path


def _repo_root() -> Path:
    """Locate the triad root: $TRIAD_HOME first, then upward from this file.

    Deliberately not $HERMES_HOME: the normal flow is Strix -> Cairn, so nothing
    here should require a Hermes install to exist.
    """
    env = os.environ.get("TRIAD_HOME")
    if env:
        return Path(env).expanduser().resolve()
    here = Path(__file__).resolve().parent
    for cand in (here, *here.parents):
        if (cand / "plugin" / "cairn.py").is_file():
            return cand
    return here


REPO = _repo_root()
_PLUGIN = REPO / "plugin"
if not (_PLUGIN / "cairn.py").is_file():
    _PLUGIN = REPO


def _load_plugin_modules():
    """Import cairn.py / strix.py from the tool package without installing it."""
    pkg = types.ModuleType("triad_plugin")
    pkg.__path__ = [str(_PLUGIN)]
    sys.modules["triad_plugin"] = pkg
    mods = {}
    for name in ("cairn", "strix"):
        spec = importlib.util.spec_from_file_location(f"triad_plugin.{name}",
                                                      _PLUGIN / f"{name}.py")
        mod = importlib.util.module_from_spec(spec)
        sys.modules[f"triad_plugin.{name}"] = mod
        spec.loader.exec_module(mod)
        mods[name] = mod
    return mods["cairn"], mods["strix"]


cairn, strix = _load_plugin_modules()


def client():
    return cairn.Cairn(os.environ.get("CAIRN_BASE_URL", cairn.DEFAULT_BASE))


def _read_roe(path, *, max_chars=1800):
    """The ROE becomes one high-signal hint, not a hint per bullet line.

    Hints are read by every worker on every scheduling pass; a dozen fragments
    of markdown dilute the graph. One dense block stays legible.
    """
    if not path:
        return []
    text = Path(path).expanduser().read_text(encoding="utf-8").strip()
    if not text:
        return []
    if len(text) > max_chars:
        text = text[:max_chars].rstrip() + "\n...[truncated]"
    return [f"[ROE] {text}"]


def _slug(text, fallback="engagement"):
    """A filesystem-safe name for an engagement directory."""
    cleaned = re.sub(r"[^a-z0-9]+", "-", (text or "").lower()).strip("-")
    return cleaned[:48] or fallback


def _workdir_root():
    """The directory engagement subdirectories live under."""
    base = (os.environ.get("TRIAD_WORKDIR") or _env_read().get("TRIAD_WORKDIR")
            or strix.DEFAULT_WORKDIR)
    return Path(base).expanduser().resolve()


def _strix_workdir(args):
    """Where strix_runs/ lives: --workdir, else TRIAD_WORKDIR, else ~/engagements."""
    if getattr(args, "workdir", None):
        return Path(args.workdir).expanduser().resolve()
    return _workdir_root()


def _engagement_dirs():
    """Engagement directories under the root: anything holding a strix_runs/ or a pid."""
    root = _workdir_root()
    found = []
    if (root / "strix_runs").is_dir():
        found.append(root)                      # a scan run straight into the root
    if root.is_dir():
        for entry in sorted(root.iterdir()):
            if entry.is_dir() and ((entry / "strix_runs").is_dir()
                                   or (entry / strix.PID_FILE).is_file()):
                found.append(entry)
    return found


def _engage_workdir(args):
    """One engagement's own directory, under the Strix workdir.

    The wrapper and .env both set TRIAD_WORKDIR, so the layout matches what the docs and
    the plugin already assume.
    """
    if getattr(args, "workdir", None):
        return Path(args.workdir).expanduser().resolve()
    return (_strix_workdir(args) / _slug(getattr(args, "title", ""))).resolve()


def _short(n):
    """18.2M, 279k, 812: token counts read at a glance."""
    if n is None:
        return "-"
    for unit, size in (("B", 1_000_000_000), ("M", 1_000_000), ("k", 1_000)):
        if n >= size:
            return f"{n / size:.1f}{unit}".replace(".0" + unit, unit)
    return str(n)


def _elapsed(start, end=None):
    """How long a run has been going, or how long it took."""
    if not start:
        return "-"
    try:
        began = datetime.fromisoformat(start)
        if end:
            stopped = datetime.fromisoformat(end)
        else:
            stopped = datetime.now(tz=began.tzinfo) if began.tzinfo else datetime.now()
    except (ValueError, TypeError):
        return "-"
    minutes = int((stopped - began).total_seconds() // 60)
    return f"{minutes // 60}h{minutes % 60:02d}m" if minutes >= 60 else f"{minutes}m"


def _project_link(workdir):
    """The Cairn project this engagement fed into, so a dashboard can pair the two."""
    try:
        text = (Path(workdir).expanduser() / PROJECT_LINK).read_text(encoding="utf-8").strip()
    except OSError:
        return None
    return text or None


def _project_fed_at(workdir):
    """When the engagement was last fed into its project, as ISO 8601.

    The link file is written on every feed, so its mtime is the freshest handoff; no
    link file means no timestamp.
    """
    try:
        mtime = (Path(workdir).expanduser() / PROJECT_LINK).stat().st_mtime
    except OSError:
        return None
    return datetime.fromtimestamp(mtime).astimezone().isoformat()


def _feed_run(c, project, workdir, run_id=None, anchor="origin"):
    """Read a run and post its leads. Returns (run, hint_ids, intent_ids).

    The handoff the whole tool exists for: without it the graph has nothing to search.
    The posting itself lives in the plugin, shared with the Hermes front end.
    """
    run = strix.read_run(Path(workdir).expanduser(), run_id)
    posted_hints, posted_intents = strix.post_leads(c, project, run, anchor)
    try:
        (Path(workdir).expanduser() / PROJECT_LINK).write_text(project, encoding="utf-8")
    except OSError:
        pass
    return run, posted_hints, posted_intents


def cmd_engage(args):
    """The default flow: create the project, run Strix, hand the findings to Cairn.

    All three in one command is the point. A Strix run on its own is JSON nobody
    reads, and a Cairn project with no input has nothing to search, so the handoff
    between them is the part that matters. `--no-scan` stops after the project.
    """
    if not _cairn_up():
        _err(f"Cairn is not answering at {_base_url()}")
        print("     start the stack first:  triad up")
        return 2

    c = client()

    # Cairn waits for the scan: the dispatcher would otherwise bootstrap the new project on a
    # graph holding none of Strix's input. Stop it before creation, which is when it would act.
    held = False
    if not getattr(args, "no_scan", False) and not getattr(args, "no_pause", False):
        if _pid_alive(DISPATCH_PID) and _dispatcher_stop_now():
            held = True

    hints = _read_roe(args.roe) or []
    res = c.create_project(
        title=args.title,
        origin=f"target {args.target}",
        goal=args.goal,
        hints=hints,
        bootstrap_enabled=not args.no_bootstrap,
    )
    pid = res["project"]["id"]
    print(f"project {pid}: {res['project']['title']}  [{res['project']['status']}]")
    print(f"  origin: {args.target}")
    print(f"  goal:   {args.goal}")
    print(f"  hints:  {len(hints)} from {args.roe or '(none)'}")
    if held:
        print("  cairn:  dispatcher stopped; it works the graph only after the feed")

    summary = {"project": pid, "target": args.target, "goal": args.goal,
               "workdir": None, "findings": 0, "coverage_gaps": 0,
               "hints_posted": 0, "intents_posted": 0}

    if getattr(args, "no_scan", False):
        print("\n--no-scan: stopping after the project; the graph has no input yet")
        print(f"  next:  triad scan --target {args.target} --workdir <dir>")
        print(f"         triad feed --project {pid} --workdir <dir>")
        if args.json:
            print(json.dumps(summary))
        return 0

    if not strix.available():
        _err("strix is not installed, so nothing can be fed into the graph")
        print("     ./install.sh installs it; the project above still exists")
        return 2

    workdir = _engage_workdir(args)
    summary["workdir"] = str(workdir)
    print(f"\n1/2 strix {args.mode} scan (max {args.max_turns} turns) -> {workdir}")
    launch = strix.run_scan(args.target, workdir, instruction_file=args.roe,
                            scan_mode=args.mode, max_turns=args.max_turns)
    print(f"    pid {launch['pid']}; log {launch['log']}")
    print(f"    waiting up to {args.scan_timeout}s for the run to settle "
          "(it stops as soon as the process exits)")
    run_dir, why = _wait_for_run(workdir, timeout=args.scan_timeout, pid=launch["pid"])
    if run_dir is None:
        _err("strix exited without creating a run directory, so there is nothing to feed")
        hint = _strix_log_hint(launch["log"])
        if hint:
            print(f"     strix log says: {hint}")
        print(f"     full log: {launch['log']}")
        if held:
            _warn("cairn stays paused, because nothing was fed to it")
            print("     start it on the unfed graph with:  triad up")
        print(f"     if a run turns up later:  triad feed --project {pid} --workdir {workdir}")
        return 1

    print(f"    run: {run_dir.name}  ({_why_text(why, args.scan_timeout)})")
    print(f"\n2/2 feeding it into {pid}")
    before = c.get_project(pid)
    run, posted_hints, posted_intents = _feed_run(c, pid, workdir, None, args.anchor)
    after = c.get_project(pid)
    summary.update({"findings": len(run["findings"]),
                    "coverage_gaps": len(run["coverage_gaps"]),
                    "hints_posted": len(posted_hints),
                    "intents_posted": len(posted_intents)})
    print(f"    findings {len(run['findings'])}  coverage gaps {len(run['coverage_gaps'])}")
    print(f"    posted {len(posted_hints)} hints, {len(posted_intents)} intents")
    print(f"    graph now: {_graph_delta(before, after)}")
    if not posted_hints and not posted_intents:
        _warn("nothing was posted: that run has no findings and no coverage gaps to hand over")
        report = _report_pointer(run_dir)
        if report:
            print(f"     it wrote a report instead: {report}")
            print("     a report can exist with zero findings; read it before concluding anything")
        else:
            print(f"     check the run itself:  triad findings --workdir {workdir}")
    if why in ("exited", "timeout") or run.get("status") in ("running", "in_progress", None):
        _warn("that run had not settled, so re-feed once it has; hints and intents")
        print(f"     are additive:  triad feed --project {pid} --workdir {workdir}")

    # The findings are in the graph now, so Cairn can work it. Releasing it here is
    # the second half of the sequencing, not an afterthought.
    if getattr(args, "hold", False):
        print("\ncairn:  left idle (--hold); start it when ready:  triad up")
    elif _pid_alive(DISPATCH_PID):
        _ok("cairn is working the fed graph (the dispatcher was already running)")
    else:
        dpid, problem = _dispatcher_start_checked(getattr(args, "config", None))
        if problem:
            _warn(f"cairn did not start: {problem}")
            print("     start it yourself:  triad up")
        else:
            _ok(f"cairn started on the fed graph (dispatcher pid {dpid})")

    print("\nnext:")
    print(f"  triad watch  --project {pid}                 # follow the graph")
    print(f"  triad report --project {pid} --workdir {workdir} -o report.md")
    if args.json:
        print(json.dumps(summary))
    if getattr(args, "watch", False):
        return _watch_until(pid, args.watch_timeout, args.interval)
    return 0


def cmd_scan(args):
    workdir = Path(args.workdir).expanduser()
    res = strix.run_scan(args.target, workdir,
                         instruction_file=args.roe,
                         scan_mode=args.mode,
                         max_turns=args.max_turns)
    print(f"strix started pid={res['pid']} in {res['cwd']}")
    print(f"  cmd: {' '.join(res['cmd'])}")
    if not args.wait:
        print("  (not waiting; watch it with: triad progress, or triad view for the dashboard)")
        return 0
    print("  waiting for the run directory to appear and settle...")
    run_dir, why = _wait_for_run(workdir, timeout=args.wait_timeout, pid=res["pid"])
    if run_dir is None:
        _err("strix exited without creating a run directory")
        hint = _strix_log_hint(res["log"])
        if hint:
            print(f"     strix log says: {hint}")
        print(f"     full log: {res['log']}")
        return 1
    print(f"  run dir: {run_dir}  ({_why_text(why, args.wait_timeout)})")
    return 0


def _pid_state(pid):
    """'running', 'stopped', 'gone', or 'unknown'. A zombie counts as gone: never reaped.

    The scan is launched detached and never waited on, so when it exits it stays a zombie
    for as long as this process lives, and `os.kill(pid, 0)` keeps succeeding on it.
    """
    if not pid or pid <= 0:
        return "gone"
    stat = Path(f"/proc/{pid}/stat")
    if not stat.exists():
        if Path("/proc").is_dir():
            return "gone"                       # /proc exists and has no entry for it
        try:
            os.kill(pid, 0)                     # no /proc (macOS): best effort
            return "running"
        except ProcessLookupError:
            return "gone"
        except PermissionError:
            return "running"
    try:
        state = stat.read_text(encoding="utf-8", errors="replace").rpartition(")")[2].split()[0]
    except (OSError, IndexError):
        return "unknown"
    if state == "Z":
        return "gone"
    return "stopped" if state in ("T", "t") else "running"


def _run_status(run_dir):
    """The status run.json reports, or None when it is missing or unreadable."""
    try:
        return json.loads((run_dir / "run.json").read_text(encoding="utf-8")).get("status")
    except (json.JSONDecodeError, OSError):
        return None


def _newest_mtime(path):
    """The most recent write anywhere under `path`, or 0 when nothing can be read."""
    newest = 0.0
    for entry in path.rglob("*"):
        try:
            newest = max(newest, entry.stat().st_mtime)
        except OSError:
            continue
    return newest


def _wait_for_run(workdir, timeout, pid=None, quiet=180):
    """Wait for a launched scan to settle. Returns (run_dir, reason).

    Waiting on run.json's status alone outlives the scan: a killed run, or one whose final
    status write never lands, leaves `status: running` for good, so the wait runs its whole
    timeout while the process has been gone for an hour. Three better signals, in order:

      finished  run.json reached a terminal status
      exited    the process we launched is gone (the zombie still answers kill(pid, 0))
      quiet     results exist and nothing has been written for `quiet` seconds
      timeout   still running; feed what is there so far, since re-feeding is additive
    """
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        run_dir = strix.latest_run_dir(workdir)
        if run_dir:
            last = run_dir
            status = _run_status(run_dir)
            if status and status not in ("running", "in_progress", None):
                return run_dir, "finished"
        if pid and _pid_state(pid) == "gone":
            return last, "exited" if last else "no-run"
        if last:
            has_results = ((last / "vulnerabilities.json").is_file()
                           or (last / "findings.sarif").is_file())
            newest = _newest_mtime(last)
            if has_results and newest and (time.time() - newest) >= quiet:
                return last, "quiet"
        time.sleep(10)
    return last, "timeout"


def _why_text(why, timeout=None):
    """Why the wait stopped, in words that say what to do about it."""
    return {
        "finished": "run.json reports it finished",
        "exited": "the Strix process exited while run.json still said running; feeding what is on disk",
        "quiet": "results stopped changing, so the run is treated as settled",
        "timeout": f"still running after {timeout}s; feeding what is there so far",
    }.get(why, why)


def _report_pointer(run_dir):
    """The markdown report Strix writes, when it wrote one.

    A run can finish and produce a report that says the assessment never got started
    (`--max-turns` too low, a blocked target), so a report on disk is not evidence of a
    finding, and its absence is not evidence of a clean target either.
    """
    report = Path(run_dir) / "penetration_test_report.md"
    return report if report.is_file() else None


def _graph_delta(before, after):
    """What a feed changed, counted from the graph Cairn returns rather than assumed.

    get_project answers with {project, facts, hints, intents}; the *_count fields only
    exist on the list endpoint, so count the lists.
    """
    parts = []
    for label in ("hints", "intents", "facts"):
        was = len(before.get(label) or [])
        now = len(after.get(label) or [])
        parts.append(f"{label} {now} (+{now - was})")
    return "  ".join(parts)


def cmd_findings(args):
    run = strix.read_run(Path(args.workdir).expanduser(), args.run)
    print(f"run: {run['run']}  status={run['status']}  cost=${run['cost_usd']}  turns={run['turns']}")
    print(f"findings: {len(run['findings'])}   coverage gaps: {len(run['coverage_gaps'])}")
    for f in run["findings"]:
        print(f"  [{f.get('severity')}] {f.get('title')} @ {f.get('target')}")
    for g in run["coverage_gaps"]:
        print(f"  [gap] {g.get('message') or g.get('rule')}")
    return 0


def _progress_line(p):
    """One line worth of a run's progress, for `progress` and its follow loop."""
    a, t, u = p["agents"], p["todos"], p["usage"]
    return (f"agents {a['completed']}/{a['total']} done  todos {t['done']}/{t['total']}  "
            f"notes {p['notes']}  findings {p['findings']}  "
            f"tokens {_short(u['input_tokens'])}/{_short(u['output_tokens'])}  "
            f"requests {u['requests']}")


def _message_line(m):
    """One compact line for a verbose stream entry, so a long run still reads."""
    kind = {"function_call": f"call {m.get('tool') or '?'}",
            "function_call_output": "result"}.get(m.get("type"), m.get("type"))
    who = m.get("agent_name") or m.get("session_id") or "?"
    text = " ".join((m.get("text") or "").split())
    if len(text) > 160:
        text = text[:159] + "…"
    stamp = (m.get("at") or "")[11:19]
    return f"  {stamp} {str(kind):<18} {who}: {text}"


def _print_progress_detail(p):
    """The verbose human view: the agent stream first, then the run's other state."""
    print(f"{p['run']}  {p['status']}  {_elapsed(p['start_time'], p['end_time'])}")
    print(f"  {_progress_line(p)}")

    messages = p.get("messages") or []
    print(f"\nmessages ({len(messages)})")
    if not messages:
        print("  (none yet)")
    for m in messages:
        print(_message_line(m))

    print("\nagents")
    for a in p.get("agents_detail") or []:
        pending = f"  pending {a['pending']}" if a.get("pending") else ""
        print(f"  {a['status']:<10} {a['name']}{pending}")

    if p.get("todos_detail") is not None:
        print("\ntodos")
        for t in p["todos_detail"]:
            print(f"  {t['status']:<11} {t['title']}  ({t['agent_name']})")

    if p.get("findings_detail"):
        print("\nfindings")
        for f in p["findings_detail"]:
            print(f"  [{f['severity']}] {f['title']}")

    if p.get("coverage") is not None:
        cov = p["coverage"]
        print("\ncoverage")
        if cov.get("summary") is not None:
            print(f"  summary: {json.dumps(cov['summary'], default=str)[:240]}")
        if "gaps" in cov:
            print(f"  gaps: {len(cov['gaps'])}")

    if p.get("notes_detail"):
        print("\nnotes")
        for n in p["notes_detail"]:
            print(f"  {n['title']}  ({n['agent_name']})")

    tail = p.get("log_tail") or []
    print(f"\nlog tail ({len(tail)} lines)")
    for line in tail:
        print(f"  {line}")


def cmd_progress(args):
    """How far along a scan is, from the state Strix writes while it runs.

    A headless scan prints nothing, so this reads the same files its own viewer does.
    """
    workdir = _strix_workdir(args)
    verbose = getattr(args, "verbose", False)
    try:
        if verbose:
            p = strix.run_progress_detail(workdir, getattr(args, "run", None),
                                          log_lines=getattr(args, "log_lines", 200),
                                          messages=getattr(args, "messages", 200))
        else:
            p = strix.run_progress(workdir, getattr(args, "run", None))
    except FileNotFoundError as e:
        print(f"  {e}")
        return 1
    if args.json:
        print(json.dumps(p, indent=2))
        return 0
    if verbose:
        _print_progress_detail(p)
        return 0

    print(f"{p['run']}  {p['status']}  {_elapsed(p['start_time'], p['end_time'])}")
    print(f"  {_progress_line(p)}")
    if p["agents"]["running"]:
        print(f"  now: {', '.join(p['agents']['running'][:3])}")
    if p["agents"]["failed"]:
        print(f"  failed agents: {p['agents']['failed']}")
    if not args.follow:
        print(f"  live dashboard: triad view --workdir {workdir}")
        return 0

    try:
        while p["status"] == "running":
            time.sleep(args.interval)
            p = strix.run_progress(workdir, getattr(args, "run", None))
            print(f"  {time.strftime('%H:%M:%S')}  {p['status']}  {_progress_line(p)}")
    except KeyboardInterrupt:
        print()
        return 0
    print(f"  run is {p['status']}: {_elapsed(p['start_time'], p['end_time'])}")
    return 0


def cmd_view(args):
    """Open Strix's own dashboard for a run, live or finished."""
    workdir = _strix_workdir(args)
    try:
        run_dir = strix.resolve_run(workdir, getattr(args, "run", None))
    except FileNotFoundError as e:
        print(f"  {e}")
        return 1
    note = strix.fix_viewer_config()
    if note:
        print(f"  {note}")

    cmd = [strix._resolve_bin(), "view", run_dir.name]
    if args.port:
        cmd += ["--port", str(args.port)]
    if args.host:
        cmd += ["--host", args.host]
    if args.no_open:
        cmd.append("--no-open")
    print(f"  {run_dir.name}: starting the viewer, Ctrl-C to stop")
    print("  the URL it prints carries a token that can steer the run: do not share it")
    try:
        return subprocess.call(cmd, cwd=str(workdir))
    except KeyboardInterrupt:
        return 0


def _find_scan_pid(workdir):
    """A strix process whose working directory is this engagement.

    The fallback for a scan that recorded no pid: started before that was written, or
    started by something else. Without it those runs are visible but uncontrollable.
    """
    target = Path(workdir).expanduser().resolve()
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit():
            continue
        try:
            cmdline = (entry / "cmdline").read_bytes().replace(b"\0", b" ").decode("utf-8", "replace")
            if "strix" not in cmdline or (entry / "cwd").resolve() != target:
                continue
        except (OSError, ValueError):
            continue
        return int(entry.name)
    return None


def _signal_scan(pid, sig):
    """Signal the scan's whole process group, falling back to the process alone."""
    try:
        os.killpg(os.getpgid(pid), sig)
    except OSError:
        os.kill(pid, sig)


def _snapshot():
    """Everything a dashboard needs in one call: runs, projects, dispatcher.

    The TUI polls this instead of re-deriving state, so the CLI and the dashboard cannot
    disagree about what is running. The recorded pid belongs to the workdir's newest run,
    which is the one a launch from that directory produced.
    """
    cairn_up = _cairn_up()
    projects = []
    if cairn_up:
        try:
            projects = client().list_projects()
        except (urllib.error.URLError, OSError, ValueError):
            projects = []

    runs = []
    for workdir in _engagement_dirs():
        pid = strix.read_pid(workdir) or _find_scan_pid(workdir)
        state = _pid_state(pid) if pid else "gone"
        live = state in ("running", "stopped")
        project = _project_link(workdir)
        fed_at = _project_fed_at(workdir)
        runs_dir = workdir / "strix_runs"
        if not runs_dir.is_dir():
            continue
        dirs = [d for d in runs_dir.iterdir() if d.is_dir()]
        for index, run_dir in enumerate(sorted(dirs, key=lambda d: d.stat().st_mtime, reverse=True)):
            try:
                progress = strix.run_progress(workdir, run_dir.name)
            except (OSError, ValueError):
                continue
            current = index == 0 and live
            progress.update({"workdir": str(workdir), "live": current, "project": project,
                             "pid": pid if current else None,
                             "paused": current and state == "stopped"})
            if fed_at:
                progress["project_fed_at"] = fed_at
            runs.append(progress)

    dispatcher = _pid_alive(DISPATCH_PID)
    return {
        "root": str(_workdir_root()),
        "cairn": {"base": _base_url(), "up": cairn_up, "projects": projects},
        "dispatcher": {"pid": dispatcher, "alive": bool(dispatcher)},
        "runs": runs,
    }


def cmd_runs(args):
    """Every run triad knows about, newest first. The dashboard opens on this."""
    snapshot = _snapshot()
    if args.json:
        print(json.dumps(snapshot, indent=2))
        return 0

    cairn = snapshot["cairn"]
    print(f"cairn       {cairn['base']}  {'up' if cairn['up'] else 'DOWN'}  "
          f"{len(cairn['projects'])} project(s)")
    print("dispatcher  " + (f"alive, pid {snapshot['dispatcher']['pid']}"
                            if snapshot["dispatcher"]["alive"] else "stopped"))
    print(f"root        {snapshot['root']}")
    if not snapshot["runs"]:
        print("\nno Strix runs yet")
        return 0
    print()
    for r in snapshot["runs"]:
        state = "paused" if r["paused"] else ("running" if r["live"] else (r["status"] or "?"))
        print(f"  {r['run'][:32]:<32} {state:<9} {_elapsed(r['start_time'], r['end_time']):>7}  "
              f"find {r['findings']:<3} gaps {r['coverage_gaps']:<3} "
              f"agents {r['agents']['completed']}/{r['agents']['total']}  "
              f"{r['project'] or '-':<10} {r['workdir']}")
    return 0


def cmd_control(args):
    """Pause, resume, stop or delete a scan or a project. The dashboard's keys call this."""
    action = args.action

    if args.project:
        c = client()
        try:
            if action == "delete":
                c.delete_project(args.project)
                _ok(f"deleted project {args.project}")
            elif action in ("pause", "stop"):
                c.set_status(args.project, "stopped")
                _ok(f"{args.project} stopped: exploration writes are rejected until it resumes")
            elif action == "resume":
                c.set_status(args.project, "active")
                _ok(f"{args.project} is active again")
            else:
                _err(f"'{action}' is not a project action")
                return 2
        except Exception as e:
            _err(f"{action} failed: {e}")
            return 1
        return 0

    if not args.workdir:
        _err("nothing to act on: give --workdir for a scan, or --project for a project")
        return 2
    workdir = Path(args.workdir).expanduser().resolve()
    pid = strix.read_pid(workdir) or _find_scan_pid(workdir)
    state = _pid_state(pid) if pid else "gone"

    if action == "delete":
        if not args.run:
            _err("deleting a run needs --run <name>")
            return 2
        target = workdir / "strix_runs" / args.run
        if not target.is_dir() or target.parent.name != "strix_runs":
            _err(f"not a run directory: {target}")
            return 1
        # Deleting a run needs no live process, but a live one must be stopped first:
        # removing the directory under it leaves it writing to a path that no longer
        # exists. A SIGSTOPped scan ignores SIGTERM until it is continued, so it gets
        # SIGCONT and then SIGTERM.
        if state == "running":
            _signal_scan(pid, signal.SIGTERM)
            prefix = f"stopped the scan (pid {pid}), then "
        elif state == "stopped":
            _signal_scan(pid, signal.SIGCONT)
            _signal_scan(pid, signal.SIGTERM)
            prefix = f"continued and stopped the scan (pid {pid}), then "
        else:
            prefix = ""
        shutil.rmtree(target)
        _ok(f"{prefix}deleted run {args.run} (its findings are gone; a fed graph keeps its hints)")
        return 0

    if state == "gone":
        _err(f"no live scan recorded in {workdir}")
        return 1

    if action == "pause":
        _signal_scan(pid, signal.SIGSTOP)
        _ok(f"paused the scan (pid {pid}); resume:  triad control resume --workdir {workdir}")
    elif action == "resume":
        _signal_scan(pid, signal.SIGCONT)
        _ok(f"resumed the scan (pid {pid})")
    else:
        _signal_scan(pid, signal.SIGTERM)
        _ok(f"stopped the scan (pid {pid}); its artifacts stay in {workdir}/strix_runs")
    return 0


def cmd_tui(args):
    """Open the dashboard: every run, its progress, its telemetry, its controls."""
    tui = _repo_root() / "tui"
    if not (tui / "node_modules" / "ink").is_dir():
        _err("the dashboard's dependencies are not installed yet")
        print("     run ./install.sh (it installs Node and the dashboard; --no-tui skips it)")
        return 1
    node = _node_bin()
    if not node:
        _err("node was not found; the dashboard needs Node 18 or newer")
        print("     run ./install.sh (it installs Node and writes it to your shell rc; --no-tui skips it)")
        return 1
    version = _node_version(node)
    major = _node_major(version)
    if major is None or major < 18:
        _err(f"node {version or 'unknown'} at {node} is older than 18; the dashboard needs 18 or newer")
        return 1
    cmd = [node, "--import", "tsx/esm", str(tui / "src" / "index.tsx")]
    if getattr(args, "interval", None):
        cmd += ["--interval", str(args.interval)]
    # The app shells out to this CLI for its state, so it has to be told where it is:
    # its cwd is the tui directory, where no triad.py exists.
    env = dict(os.environ)
    env["TRIAD_PY"] = str(_repo_root() / "triad.py")
    env.setdefault("TRIAD_PYTHON", sys.executable)
    try:
        return subprocess.call(cmd, cwd=str(tui), env=env)
    except KeyboardInterrupt:
        return 0


def cmd_feed(args):
    c = client()
    before = c.get_project(args.project)
    run, posted_hints, posted_intents = _feed_run(
        c, args.project, args.workdir, args.run, args.anchor)
    after = c.get_project(args.project)
    print(f"fed run {run['run']} -> {args.project}")
    print(f"  findings: {len(run['findings'])}  gaps: {len(run['coverage_gaps'])}")
    print(f"  posted {len(posted_hints)} hints, {len(posted_intents)} intents")
    print(f"  graph now: {_graph_delta(before, after)}")
    if not posted_hints and not posted_intents:
        _warn("nothing was posted: that run has no findings and no coverage gaps to hand over")
        report = _report_pointer(run.get("run_dir") or "")
        if report:
            print(f"  it wrote a report instead: {report}")
            print("  a report can exist with zero findings; read it before concluding anything")
    return 0


def _watch_until(project, timeout, interval):
    """Poll the graph until it completes, is stopped, or the timeout runs out."""
    c = client()
    deadline = time.time() + timeout
    last_sig = None
    while time.time() < deadline:
        g = c.get_project(project)
        p, facts, intents = g["project"], g.get("facts", []), g.get("intents", [])
        open_i = [i for i in intents if not i.get("to")]
        sig = (p["status"], len(facts), len(open_i))
        if sig != last_sig:
            print(f"[{time.strftime('%H:%M:%S')}] status={p['status']} "
                  f"facts={len(facts)} open_intents={len(open_i)}")
            last_sig = sig
        if p["status"] == "completed":
            print("goal reached.")
            return 0
        if p["status"] == "stopped":
            print("project was stopped.")
            return 2
        time.sleep(interval)
    print(f"timed out after {timeout}s; last state: {last_sig}")
    return 1


def cmd_watch(args):
    return _watch_until(args.project, args.timeout, args.interval)


def cmd_status(args):
    c = client()
    if args.project:
        g = c.get_project(args.project)
        print(json.dumps(c.summarize(g), indent=2, ensure_ascii=False))
        if args.path:
            print("\nattack path:")
            for step in c.goal_path(g):
                print(f"  {step['fact']:8} <- {step['via']}   :: {step['description']}")
    else:
        for p in c.list_projects():
            print(f"{p['id']:10} {p['status']:10} facts={p.get('fact_count',0):3} "
                  f"open={p.get('unclaimed_intent_count',0)+p.get('working_intent_count',0):3} "
                  f"{p['title']}")
    return 0


def cmd_report(args):
    c = client()
    g = c.get_project(args.project)
    p = g["project"]
    out = [f"# Pentest report: {p['title']}", "",
           f"- Status: **{p['status']}**", f"- Project: `{p['id']}`",
           f"- Started: {p.get('created_at')}", ""]

    origin = next((f["description"] for f in g["facts"] if f["id"] == "origin"), "")
    goal = next((f["description"] for f in g["facts"] if f["id"] == "goal"), "")
    out += ["## Objective", "", f"- Origin: {origin}", f"- Goal: {goal}", ""]

    path = c.goal_path(g)
    if path:
        out += ["## Attack path", ""]
        for step in path:
            via = step["via"] or "(starting point)"
            out += [f"1. **{step['fact']}**: {step['description']}",
                    f"   - via: {via}  (worker: {step['worker'] or 'n/a'})"]
        out.append("")
    else:
        out += ["## Attack path", "", "_No completed path: the goal was not reached._", ""]

    out += ["## Facts", ""]
    for f in g["facts"]:
        if f["id"] not in ("origin", "goal"):
            out.append(f"- `{f['id']}` {f['description']}")
    out += ["", "## Open intents (unresolved directions)", ""]
    open_i = [i for i in g.get("intents", []) if not i.get("to")]
    out += ([f"- `{i['id']}` {i['description']} (worker: {i.get('worker') or 'unclaimed'})"
             for i in open_i] or ["_none_"])
    out += ["", "## Hints injected", ""]
    out += ([f"- {h['content']}" for h in g.get("hints", [])] or ["_none_"])

    if args.workdir:
        try:
            run = strix.read_run(Path(args.workdir).expanduser(), args.run)
            out += ["", "## Discovery coverage (Strix)", "",
                    f"- Run: `{run['run']}` status={run['status']} cost=${run['cost_usd']}",
                    f"- Validated findings: {len(run['findings'])}",
                    f"- Coverage gaps (UNEXAMINED, not clean): {len(run['coverage_gaps'])}", ""]
            for gp in run["coverage_gaps"]:
                out.append(f"  - {gp.get('message') or gp.get('rule')} ({gp.get('location','')})")
        except Exception as exc:  # noqa: BLE001
            out += ["", f"_Strix run not readable: {exc}_"]

    text = "\n".join(out) + "\n"
    if args.output:
        Path(args.output).expanduser().write_text(text, encoding="utf-8")
        print(f"wrote {args.output}")
    else:
        print(text)
    return 0


# env, stack control and the first-run wizard: collect keys into .env, bring Cairn up, start
# the dispatcher and report readiness. Runtime state lives in .triad/, which is gitignored.

STATE_DIR = REPO / ".triad"
SERVER_PID = STATE_DIR / "server.pid"
DISPATCH_PID = STATE_DIR / "dispatcher.pid"

# Written into an engagement directory when its run is fed, so the pairing survives.
PROJECT_LINK = ".triad-project"
SERVER_LOG = STATE_DIR / "server.log"
DISPATCH_LOG = STATE_DIR / "dispatcher.log"
CAIRN_DIR = REPO / "cairn"
# The shipped dispatcher config is the template; the machine-specific one is written
# into .triad/ so a chosen worker model never modifies a tracked file.
LOCAL_TEMPLATE = REPO / "dispatch.local.yaml"
LOCAL_OVERRIDE = STATE_DIR / "dispatch.local.yaml"
DEFAULT_BASE = "http://127.0.0.1:8000"
WORKER_CLIS = ("opencode", "claude", "codex", "pi")

# One provider drives both layers, so the wizard asks once; both id forms come from the entry's
# prefixes, and every models_url here was checked live (401 without a key is a correct route).
PROVIDERS: list[dict] = [
    {
        "name": "OpenCode Go / Zen",
        "strix_llm": "openai/deepseek-v4.1-flash",
        "base": "https://opencode.ai/zen/go/v1",
        "headers": True,                 # this endpoint rejects a bare client
        "worker_model": "opencode-go/deepseek-v4.1-flash",
        "key_var": "OPENCODE_GO_API_KEY",
        "also": [],
        "litellm_prefix": "openai",      # Strix reaches it as an OpenAI-compatible API
        "opencode_prefix": "opencode-go",
        "models_url": "https://opencode.ai/zen/go/v1/models",
        "auth": "bearer",
    },
    {
        "name": "OpenRouter",
        "strix_llm": "openrouter/z-ai/glm-5.3",
        "base": None,
        "headers": False,
        "worker_model": "openrouter/z-ai/glm-5.3",
        "key_var": "OPENROUTER_API_KEY",
        "also": [],
        "litellm_prefix": "openrouter",
        "models_url": "https://openrouter.ai/api/v1/models",
        "auth": "none",                  # its model list needs no key
    },
    {
        "name": "DeepSeek",
        "strix_llm": "deepseek/deepseek-chat",
        "base": None,
        "headers": False,
        "worker_model": "deepseek/deepseek-chat",
        "key_var": "DEEPSEEK_API_KEY",
        "also": [],
        "litellm_prefix": "deepseek",
        "models_url": "https://api.deepseek.com/models",
        "auth": "bearer",
    },
    {
        "name": "Anthropic",
        "strix_llm": "anthropic/claude-sonnet-4-5",
        "base": None,
        "headers": False,
        "worker_model": "anthropic/claude-sonnet-4-5",
        "key_var": "ANTHROPIC_API_KEY",
        "also": ["ANTHROPIC_AUTH_TOKEN"],   # the container workers read this one
        "litellm_prefix": "anthropic",
        "models_url": "https://api.anthropic.com/v1/models",
        "auth": "x-api-key",
    },
]
PROVIDER_CHOICES: list[tuple[str, dict]] = [
    ("OpenCode Go / Zen  opencode-go/deepseek-v4.1-flash   (subscription: one key for both)",
     PROVIDERS[0]),
    ("OpenRouter         openrouter/z-ai/glm-5.3           (billed per token)",
     PROVIDERS[1]),
    ("DeepSeek           deepseek/deepseek-chat", PROVIDERS[2]),
    ("Anthropic          anthropic/claude-sonnet-4-5", PROVIDERS[3]),
]
KEEP = "__keep__"
OTHER = "__other__"
BIND_CHOICES: list[tuple[str, str]] = [
    ("127.0.0.1  loopback only (safer)", "127.0.0.1"),
    ("0.0.0.0    reachable from the LAN (convenient, note the exposure)", "0.0.0.0"),
]


LIST_MAX = 25      # models shown at once
LIST_ALL = 40      # list at most this many without asking for a search term


def _default_raw(provider):
    """A provider's own id for its default model, with the layer prefixes removed.

    Derived rather than stored a third time, so the two prefixed forms and this one
    cannot drift apart.
    """
    model = provider["worker_model"]
    prefix = provider.get("opencode_prefix") or provider["litellm_prefix"]
    return model.split("/", 1)[1] if model.startswith(prefix + "/") else model


def _strix_model_id(provider, raw):
    """The LiteLLM id Strix needs for a model the provider calls `raw`."""
    return f"{provider['litellm_prefix']}/{raw}"


def _worker_model_id(provider, raw):
    """The opencode id the Cairn worker needs for the same model."""
    prefix = provider.get("opencode_prefix") or provider["litellm_prefix"]
    return f"{prefix}/{raw}"


def _fetch_models(provider, key=None, timeout=20):
    """Ask a provider which models it serves. Returns (ids, problem).

    Best effort on purpose: a wizard that cannot reach the provider still has to be
    usable, so every failure comes back as a reason to fall back to typing an id and
    never as an exception.
    """
    url = (provider or {}).get("models_url")
    if not url:
        return [], "this provider publishes no list"
    headers = {"User-Agent": "triad"}
    style = provider.get("auth", "bearer")
    if style == "none":
        pass
    elif style == "x-api-key":
        if not key:
            return [], "a key is needed to list these models"
        headers["x-api-key"] = key
        headers["anthropic-version"] = "2023-06-01"
    else:
        if not key:
            return [], "a key is needed to list these models"
        headers["Authorization"] = f"Bearer {key}"
    if provider.get("headers"):
        # the endpoint that rejects a bare client for completions rejects it here too
        headers["User-Agent"] = "strix-agent"
        headers["x-opencode-session"] = f"triad-{uuid.uuid4()}"
    try:
        request = urllib.request.Request(url, headers=headers, method="GET")
        with urllib.request.urlopen(request, timeout=timeout) as response:
            payload = json.loads(response.read().decode("utf-8", "replace"))
    except urllib.error.HTTPError as exc:
        return [], f"HTTP {exc.code}"
    except (urllib.error.URLError, OSError) as exc:
        return [], f"could not reach it ({exc})"
    except json.JSONDecodeError:
        return [], "the reply was not JSON"
    items = payload.get("data") if isinstance(payload, dict) else payload
    if not isinstance(items, list):
        return [], "the reply was not a model list"
    ids = sorted({str(item.get("id") or item.get("name")) for item in items
                  if isinstance(item, dict) and (item.get("id") or item.get("name"))})
    return (ids, None) if ids else ([], "the list came back empty")


def _pick_model(provider, key, side, default_raw=None):
    """Let the user choose from what the provider actually serves.

    Typing a model id from memory is how a scan dies on a model that does not exist;
    the live list removes the guess. Falls back to a plain prompt when the list is
    unavailable, so a network problem never blocks setup. Returns the provider's own
    id, or CANCELLED.
    """
    default_raw = default_raw or _default_raw(provider)
    ids, problem = _fetch_models(provider, key)
    if problem:
        _warn(f"cannot list {provider['name']} models: {problem}")
        return _ask(f"{side} model id", default_raw)
    _ok(f"{len(ids)} models available from {provider['name']}")
    while True:
        list_all = len(ids) <= LIST_ALL
        hint = "blank lists them all" if list_all else f"blank keeps {default_raw}"
        needle = _ask(f"Search the models ({hint})", "")
        if isinstance(needle, _Cancelled):
            return _Cancelled
        if not needle and not list_all:
            return default_raw
        matches = [i for i in ids if needle.lower() in i.lower()] if needle else list(ids)
        if not matches:
            _warn(f"nothing matches {needle!r}")
            continue
        if default_raw in matches:          # so Enter keeps the sensible default
            matches.remove(default_raw)
            matches.insert(0, default_raw)
        options = [(m, m) for m in matches[:LIST_MAX]]
        options.append(("Type a model id myself", OTHER))
        options.append((f"Keep {default_raw}", KEEP))
        if len(matches) > LIST_MAX:
            print(f"    ({len(matches)} matches; showing the first {LIST_MAX}, type "
                  f"more of the name to narrow, or search again)")
        pick = _choose(f"Which model should {side} use?", options, default=1)
        if isinstance(pick, _Cancelled):
            return _Cancelled
        if pick is KEEP:
            return default_raw
        if pick is OTHER:
            return _ask(f"{side} model id", default_raw)
        return pick


def _strix_headers():
    """The LLM_EXTRA_HEADERS that an endpoint refusing a bare client needs.

    The OpenCode Go endpoint answers 403 (Cloudflare error 1010) to the stock
    python-urllib User-Agent and 400 MissingSessionID without a session header, so
    both are required. Any non-default User-Agent is accepted, which is why none is
    pinned to a version here.
    """
    return json.dumps({"User-Agent": "strix-agent",
                       "x-opencode-session": f"triad-{uuid.uuid4()}"})


def _provider_named(name):
    """Look a provider up by name or key variable, case-insensitively, for the flags."""
    wanted = (name or "").strip().lower()
    for provider in PROVIDERS:
        if wanted in (provider["name"].lower(), provider["key_var"].lower()):
            return provider
    return None


def _default_provider(current, side="strix"):
    """Which menu entry to preselect for one side of the config.

    Whatever is already in use wins, so pressing Enter keeps it: for Strix that is the
    model in .env, for the worker the model the dispatcher config runs. Failing that, a
    provider the user already has a key for, since preselecting one they cannot use
    costs them a round trip.
    """
    if side == "worker":
        in_use, field = _read_worker_model(), "worker_model"
    else:
        in_use, field = current.get("STRIX_LLM"), "strix_llm"
    if in_use:
        for index, (_label, provider) in enumerate(PROVIDER_CHOICES, 1):
            if provider[field] == in_use:
                return index
    for index, (_label, provider) in enumerate(PROVIDER_CHOICES, 1):
        if current.get(provider["key_var"]):
            return index
    return 1


_BOLD, _GRN, _YEL, _RED, _RST = "\033[1m", "\033[32m", "\033[33m", "\033[31m", "\033[0m"


class _Cancelled:
    """Sentinel returned by the prompts when the user bails out (Ctrl-C or EOF).

    A class rather than a bare object so the return type of a prompt can say what it
    actually is, and a caller that checks for it gets a narrowed type from there on.
    Distinct from a None option value, because "skip this" is a real choice in more
    than one menu.
    """

    def __repr__(self):
        return "CANCELLED"


CANCELLED = _Cancelled()


def _colour(text, code):
    return f"{code}{text}{_RST}" if sys.stdout.isatty() else text


def _hdr(text):
    print(f"\n{_colour(text, _BOLD)}")


def _ok(text):
    print(f"  {_colour('✓', _GRN)} {text}")


def _warn(text):
    print(f"  {_colour('!', _YEL)} {text}")


def _err(text):
    sys.stdout.flush()          # keep stderr's line in order with what was just printed
    print(f"  {_colour('✗', _RED)} {text}", file=sys.stderr)


def _strix_log_hint(log_path):
    """Name the reason a scan stopped, from the log it wrote, when we can recognise it.

    "Read the log" is the least useful half of information already on disk, and the
    common failures here (provider, credits, docker) each have a signature.
    """
    try:
        text = Path(log_path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return None
    signatures = (
        ("LLM CONNECTION FAILED", "the model provider refused the connection"),
        ("requires more credits", "the provider account is out of credit"),
        ("Invalid credential", "the provider rejected the key"),
        ("Cannot connect to the Docker daemon", "Docker is not running"),
        ("permission denied while trying to connect", "no permission for the Docker socket"),
        ("Max turns", "it hit the turn cap"),
    )
    for needle, plain in signatures:
        if needle in text:
            return plain
    return None


def _env_path():
    return Path(os.environ.get("TRIAD_ENV") or (REPO / ".env"))


def _env_read():
    """Parse KEY=value lines. Quotes are stripped; nothing else is interpreted."""
    path = _env_path()
    values = {}
    if not path.is_file():
        return values
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, value = line.split("=", 1)
        values[key.strip()] = value.strip().strip('"').strip("'")
    return values


def _env_write(updates):
    """Merge into .env in place, keeping comments and keys we do not manage.

    Seeded from .env.example on first write so the file explains itself, and
    chmod 600 because it holds keys.
    """
    path = _env_path()
    if path.is_file():
        lines = path.read_text(encoding="utf-8").splitlines()
    else:
        example = REPO / ".env.example"
        lines = example.read_text(encoding="utf-8").splitlines() if example.is_file() else []
        if not lines:
            lines = ["# Triad configuration. Never commit this file."]
    written, out = set(), []
    for line in lines:
        stripped = line.strip()
        if stripped and not stripped.startswith("#") and "=" in stripped:
            key = stripped.split("=", 1)[0].strip()
            if key in updates:
                out.append(f"{key}={updates[key]}")
                written.add(key)
                continue
        out.append(line)
    if not path.is_file():
        out.append("")
    for key, value in updates.items():
        if key not in written:
            out.append(f"{key}={value}")
    path.write_text("\n".join(out).rstrip("\n") + "\n", encoding="utf-8")
    path.chmod(0o600)
    return path


def _load_dotenv():
    """Put .env into the environment, without clobbering what is already set.

    docker compose reads .env on its own, but this CLI did not, so a key the
    wizard wrote would silently not apply to it.
    """
    for key, value in _env_read().items():
        os.environ.setdefault(key, value)


def _base_url():
    return (os.environ.get("CAIRN_BASE_URL") or _env_read().get("CAIRN_BASE_URL")
            or DEFAULT_BASE).rstrip("/")


def _mask(value):
    if not value:
        return "(empty)"
    if len(value) <= 8:
        return "*" * len(value)
    return f"{value[:4]}...{value[-4:]}"


def _clean(value):
    """Drop control characters and surrounding whitespace.

    A terminal left in a non-canonical state delivers backspace and interrupt keys as
    literal bytes. Those must never become part of a key value, where the failure they
    cause is an authentication error that nothing in the output explains.
    """
    return "".join(ch for ch in (value or "") if ch.isprintable()).strip()


def _tty_fd():
    """(fd, owned) for a terminal to prompt on, or (None, False) when there is none.

    stdin is used when it is a terminal. When stdin is a pipe but a terminal exists,
    /dev/tty is opened instead, which is what getpass does and what keeps the wizard
    usable when its own stdin has been redirected.
    """
    try:
        if sys.stdin.isatty():
            return sys.stdin.fileno(), False
    except (ValueError, AttributeError, OSError):
        pass
    try:
        return os.open("/dev/tty", os.O_RDWR), True
    except OSError:
        return None, False


def _echo(text):
    """Echo, but only onto a terminal: a redirected stdout should not get backspaces."""
    if sys.stdout.isatty():
        sys.stdout.write(text)
        sys.stdout.flush()


def _read_line(question, secret=False) -> "str | _Cancelled":
    """Prompt for one line, editing it ourselves.

    Not `input` or `getpass`: they leave editing to the terminal's line discipline, which
    a parent can leave switched off, so backspace arrives as a control byte inside the
    value and Ctrl-C does not interrupt. Returns the line, or CANCELLED on interrupt, on
    end of input, and on Ctrl-D at an empty prompt (Ctrl-D with text submits it).
    """
    prompt = f"  {question}: "
    fd, owned = _tty_fd()
    if fd is None:
        # No terminal at all (piped input, a CI runner, a hermetic test): there is
        # nothing to edit, so a plain read is the honest behaviour.
        try:
            if secret:
                import getpass
                return _clean(getpass.getpass(prompt))
            return _clean(input(prompt))
        except (EOFError, KeyboardInterrupt):
            print()
            return CANCELLED

    try:
        import termios
    except ImportError:
        return _clean(input(prompt))

    try:
        saved = termios.tcgetattr(fd)
    except termios.error:
        return _clean(input(prompt))

    working = list(saved)
    working[6] = list(saved[6])                      # a shallow copy shares the cc list
    # ISIG stays cleared: with it on, the driver turns Ctrl-C into a signal that only reaches this
    # process when it is in the terminal's foreground group. Handling the byte works regardless.
    working[3] = saved[3] & ~(termios.ICANON | termios.ECHO | termios.ISIG)
    working[6][termios.VMIN] = 1
    working[6][termios.VTIME] = 0

    sys.stdout.flush()          # keep the prompt after whatever was printed before it
    sys.stdout.write(prompt)
    sys.stdout.flush()
    buf = bytearray()
    try:
        termios.tcsetattr(fd, termios.TCSADRAIN, working)
        while True:
            try:
                chunk = os.read(fd, 1)
            except OSError:
                break
            if not chunk:                            # input ended
                print()
                return CANCELLED
            byte = chunk[0]
            if byte in (0x0a, 0x0d):                 # Enter
                break
            if byte in (0x08, 0x7f):                 # Backspace and DEL both erase
                if buf:
                    buf.pop()
                    _echo("\b \b")
                continue
            if byte == 0x15:                         # Ctrl-U: kill the line
                while buf:
                    buf.pop()
                    _echo("\b \b")
                continue
            if byte == 0x03:                         # Ctrl-C cancels the prompt
                raise KeyboardInterrupt
            if byte == 0x04:                         # Ctrl-D: end input, or submit
                if not buf:
                    print()
                    return CANCELLED
                break
            if byte < 0x20:                          # any other control byte: drop it
                continue
            buf.append(byte)
            _echo("*" if secret else chunk.decode("utf-8", "replace"))
    except KeyboardInterrupt:
        return CANCELLED
    finally:
        try:
            termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        except termios.error:
            pass
        if owned:
            os.close(fd)
        sys.stdout.write("\n")
        sys.stdout.flush()
    return _clean(bytes(buf).decode("utf-8", "replace"))


def _ask(question, default="") -> "str | _Cancelled":
    """One prompt. Returns the default on empty input, CANCELLED if the user bails."""
    suffix = f" [{default}]" if default else ""
    answer = _read_line(f"{question}{suffix}")
    if isinstance(answer, _Cancelled):
        return CANCELLED
    return answer or default


def _ask_secret(question) -> "str | _Cancelled":
    """Prompt for a secret, showing one asterisk per character typed."""
    return _read_line(question, secret=True)


def _choose(question, options, default=1):
    """Numbered menu. Returns the option's value, CANCELLED if the user bails."""
    print(f"  {question}")
    for index, (label, _value) in enumerate(options, 1):
        mark = "  (default)" if index == default else ""
        print(f"    {index}. {label}{mark}")
    while True:
        raw = _ask("Choose", str(default))
        if isinstance(raw, _Cancelled):
            return CANCELLED
        try:
            picked = int(raw)
        except ValueError:
            continue
        if 1 <= picked <= len(options):
            return options[picked - 1][1]
        print("    not one of the options")


def _aborted():
    print("\n  setup cancelled; nothing was changed")
    return 130


def _pid_alive(path):
    """Return the live pid recorded in `path`, else None. Never signals it."""
    if not path.is_file():
        return None
    try:
        pid = int(path.read_text(encoding="utf-8").strip())
    except (ValueError, OSError):
        return None
    if pid <= 0:
        return None
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return None
    except PermissionError:
        return pid
    return pid


def _uv_bin():
    for candidate in (shutil.which("uv"),
                      str(Path.home() / ".hermes" / "bin" / "uv"),
                      str(Path.home() / ".local" / "bin" / "uv")):
        if candidate and Path(candidate).is_file():
            return candidate
    return None


def _node_bin():
    node_dir = os.environ.get("NODE_DIR") or str(Path.home() / ".local" / "opt" / "node")
    for candidate in (shutil.which("node"),
                      str(Path(node_dir) / "bin" / "node"),
                      str(Path.home() / ".local" / "bin" / "node")):
        if candidate and Path(candidate).is_file():
            return candidate
    return None


def _node_version(node):
    try:
        out = subprocess.run([node, "--version"], stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True, check=False)
    except OSError:
        return None
    return out.stdout.strip() or None


def _node_major(version):
    # The installer parses `node --version` the same way: strip the v, take the major.
    try:
        return int(version.lstrip("v").split(".", 1)[0])
    except (AttributeError, ValueError):
        return None


def _worker_cli():
    for name in WORKER_CLIS:
        found = shutil.which(name)
        if found:
            return name, found
    home_opencode = Path.home() / ".opencode" / "bin" / "opencode"
    if home_opencode.is_file():
        return "opencode", str(home_opencode)
    return None, None


# Why Docker is unusable, so `triad up` names the problem rather than reporting
# that Docker is simply not usable: installed-but-broken is the common case.
DOCKER_STATES = {
    "absent": ("Docker is not installed",
               "./install.sh installs it"),
    "no-daemon": ("the Docker daemon is not reachable",
                  "start it:  sudo systemctl enable --now docker"),
    "no-permission": ("this user cannot use the Docker socket (not in the 'docker' group)",
                      'sudo usermod -aG docker "$USER", then log out and back in '
                      "(or run: newgrp docker)"),
    "no-compose": ("the 'docker compose' v2 plugin is missing",
                   "./install.sh ensures it (package: docker-compose-v2 or docker-compose-plugin)"),
}


def _docker_state():
    """Classify Docker: ok, absent, no-daemon, no-permission or no-compose."""
    if not shutil.which("docker"):
        return "absent"
    try:
        probe = subprocess.run(["docker", "info"], capture_output=True, text=True, timeout=25)
    except (OSError, subprocess.SubprocessError):
        return "no-daemon"
    if probe.returncode != 0:
        blob = f"{probe.stderr or ''}{probe.stdout or ''}".lower()
        if "permission denied" in blob:
            return "no-permission"
        return "no-daemon"
    if _compose_cmd() is None:
        return "no-compose"
    return "ok"


def _docker_perm_fix():
    """Right advice for a socket permission error, which depends on the group state.

    Three cases look identical from `docker info` but need different actions: the
    group was just added (re-login), the session already has it (something else
    restricts the socket), or the user was never added at all.
    """
    try:
        import grp
        entry = grp.getgrnam("docker")
    except (ImportError, KeyError, OSError):
        return ('no "docker" group exists on this host; reinstall Docker with '
                "./install.sh, or use local mode (triad up without Docker)")
    if entry.gr_gid in os.getgroups():
        return ("your session already has the docker group, so the socket itself is "
                "restricted; check:  ls -l /var/run/docker.sock")
    try:
        import getpass
        name = getpass.getuser()
    except Exception:
        name = os.environ.get("USER", "")
    if name and name in entry.gr_mem:
        return ("you are already in the 'docker' group, so membership has not reached "
                "this session yet: log out and back in (or run: newgrp docker)")
    return ('sudo usermod -aG docker "$USER", then log out and back in '
            "(or run: newgrp docker)")


def _docker_remedy(state):
    """(reason, fix) for a docker state, with the permission advice computed.

    Falls back rather than raising: this only ever runs on a diagnostic path, and a
    KeyError there would replace a fixable message with a traceback.
    """
    reason, fix = DOCKER_STATES.get(state, ("Docker is not usable", "./install.sh diagnoses it"))
    if state == "no-permission":
        fix = _docker_perm_fix()
    return reason, fix


def _docker_ok():
    return _docker_state() == "ok"


def _log_tail(path, lines=12):
    """Last non-empty lines of a background process log."""
    try:
        text = Path(path).read_text(encoding="utf-8", errors="replace")
    except OSError:
        return []
    return [line for line in text.splitlines() if line.strip()][-lines:]


def _print_tail(path, lines=12):
    """Show why a detached process died, and name the likely cause.

    Without this the caller only learns that something exited, which is the least
    useful half of the information already sitting in the log.
    """
    tail = _log_tail(path, lines)
    if not tail:
        print(f"     (no output in {path})")
        return
    for line in tail:
        print(f"     | {line}")
    blob = " ".join(tail).lower()
    if any(k in blob for k in ("mirrors.aliyun", "pypi.tuna", "failed to download",
                               "failed to fetch", "network is unreachable")):
        print("     | looks like a package-download failure. Cairn pins the Aliyun PyPI")
        print("     | mirror; retry with:  UV_DEFAULT_INDEX=https://pypi.org/simple triad up")


def _compose_cmd():
    """`docker compose` (v2 plugin) if present, else the v1 binary, else None."""
    if shutil.which("docker"):
        try:
            probe = subprocess.run(["docker", "compose", "version"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   timeout=20)
            if probe.returncode == 0:
                return ["docker", "compose"]
        except (OSError, subprocess.SubprocessError):
            pass
    if shutil.which("docker-compose"):
        return ["docker-compose"]
    return None


def _cairn_up(timeout=3):
    """True when the Cairn API answers."""
    try:
        with urllib.request.urlopen(f"{_base_url()}/projects", timeout=timeout) as response:
            return response.status == 200
    except (urllib.error.URLError, OSError, ValueError):
        return False


def _spawn(cmd, log_path, pid_path, cwd=None):
    """Start a detached background process and record its pid and log."""
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    log = open(log_path, "ab", buffering=0)
    try:
        proc = subprocess.Popen(cmd, cwd=str(cwd or REPO), stdin=subprocess.DEVNULL,
                                stdout=log, stderr=subprocess.STDOUT,
                                start_new_session=True)
    except OSError as exc:
        log.close()
        return None, str(exc)
    pid_path.write_text(f"{proc.pid}\n", encoding="utf-8")
    time.sleep(3)
    if proc.poll() is not None:
        return None, f"exited immediately (rc={proc.returncode}); see {log_path}"
    return proc.pid, None


def _stop_pid(pid_path, label, timeout=15):
    """SIGTERM a recorded pid, then SIGKILL if it will not go."""
    pid = _pid_alive(pid_path)
    if pid is None:
        pid_path.unlink(missing_ok=True)
        return False
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pid_path.unlink(missing_ok=True)
        return False
    deadline = time.time() + timeout
    while time.time() < deadline:
        if _pid_alive(pid_path) is None:
            break
        time.sleep(0.5)
    else:
        _warn(f"{label} ignored SIGTERM; killing it")
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        time.sleep(0.5)
    pid_path.unlink(missing_ok=True)
    return True


def _wait_for_cairn(timeout=180):
    """Poll the API while a container or build comes up, so callers can report."""
    print("    waiting for the Cairn API", end="", flush=True)
    deadline = time.time() + timeout
    while time.time() < deadline:
        if _cairn_up():
            print(" up")
            return True
        print(".", end="", flush=True)
        time.sleep(3)
    print(" timed out")
    return False


def _cairn_project():
    """The directory uv should treat as the Cairn project.

    The clone has a top level (docs, compose file, cli.py) and the actual Python
    project one level down, so `--project cairn` from the repo root finds no
    pyproject.toml and uv fails to spawn `cairn`.
    """
    for candidate in (CAIRN_DIR / "cairn", CAIRN_DIR):
        if (candidate / "pyproject.toml").is_file():
            return candidate
    return None


def _server_local_start():
    """No-Docker fallback: run the Cairn server as a host process."""
    uv = _uv_bin()
    if uv is None:
        return None, "uv not found; ./install.sh installs it (or see https://docs.astral.sh/uv/)"
    project = _cairn_project()
    if project is None:
        return None, f"no Cairn checkout at {CAIRN_DIR}; run ./install.sh"
    cmd = [uv, "run", "--project", str(project),
           "cairn", "serve", "--no-access-log"]
    return _spawn(cmd, SERVER_LOG, SERVER_PID)


def _dispatcher_start(config):
    """Start the Cairn dispatcher on the host, which is the verified path.

    The containerised dispatcher needs the amd64-only worker image (and a worker
    CLI inside it), so the host process is the default on every architecture and
    `triad up --container` opts into compose instead.
    """
    uv = _uv_bin()
    if uv is None:
        return None, "uv not found; ./install.sh installs it (or see https://docs.astral.sh/uv/)"
    project = _cairn_project()
    if project is None:
        return None, f"no Cairn checkout at {CAIRN_DIR}; run ./install.sh"
    cfg = Path(config)
    if not cfg.is_absolute():
        cfg = REPO / cfg
    if not cfg.is_file():
        return None, f"no dispatcher config at {cfg}"
    cmd = [uv, "run", "--project", str(project),
           "cairn", "dispatch", "--config", str(cfg)]
    return _spawn(cmd, DISPATCH_LOG, DISPATCH_PID)


def _dispatcher_config_path(explicit=None):
    """Which dispatcher config to run: explicit > machine-specific > shipped template.

    Machine-specific settings live under .triad/ (gitignored) so that choosing a
    worker model never leaves a tracked file modified, which would show up as a local
    change and conflict on the next pull.
    """
    if explicit:
        return str(explicit)
    if LOCAL_OVERRIDE.is_file():
        return str(LOCAL_OVERRIDE)
    return "dispatch.local.yaml"


def _write_worker_model(model):
    """Set the local worker's model in the machine-specific dispatcher config.

    Substituted into the shipped template rather than emitted from a copy in code, so
    the template's comments and settings stay the one place they are maintained.
    Returns (path, None) or (None, why it could not be written).
    """
    if not LOCAL_TEMPLATE.is_file():
        return None, f"no dispatcher template at {LOCAL_TEMPLATE}"
    text = LOCAL_TEMPLATE.read_text(encoding="utf-8")
    updated, count = re.subn(r'(?m)^(\s*OPENCODE_MODEL:\s*).*$',
                             lambda match: f'{match.group(1)}"{model}"', text)
    if not count:
        return None, "the shipped dispatch.local.yaml has no OPENCODE_MODEL line to set"
    header = (
        "# Generated by 'triad configure' from dispatch.local.yaml, which is the\n"
        "# template. Machine-specific worker settings live here so the tracked file\n"
        "# stays clean; .triad/ is gitignored, so this never conflicts on a pull.\n"
        "# Change it with:  triad configure\n\n"
    )
    LOCAL_OVERRIDE.parent.mkdir(parents=True, exist_ok=True)
    LOCAL_OVERRIDE.write_text(header + updated, encoding="utf-8")
    return str(LOCAL_OVERRIDE), None


def _read_worker_model():
    """The worker model in whichever dispatcher config would actually run."""
    path = Path(_dispatcher_config_path())
    if not path.is_absolute():
        path = REPO / path
    try:
        text = path.read_text(encoding="utf-8")
    except OSError:
        return None
    found = re.search(r'(?m)^\s*OPENCODE_MODEL:\s*"?([^"\n]+?)"?\s*$', text)
    return found.group(1) if found else None


def _dispatcher_stop_now():
    """Stop the dispatcher if it is running; True when one was actually stopped."""
    if _pid_alive(DISPATCH_PID) is None:
        return False
    return _stop_pid(DISPATCH_PID, "the dispatcher")


def _dispatcher_start_checked(config=None):
    """Start the dispatcher after the checks `triad up` makes. Returns (pid, problem).

    The worker and credential checks matter more here than in `up`, because this runs
    unattended at the end of a scan: a dispatcher with no working worker claims
    intents and then fails them, which is worse than not starting.
    """
    pid = _pid_alive(DISPATCH_PID)
    if pid:
        return pid, None
    worker, _where = _worker_cli()
    if worker is None:
        return None, "no worker CLI found (opencode, claude, codex or pi)"
    if worker == "opencode" and not _opencode_credentials():
        return None, "opencode has no credentials; run: triad auth"
    return _dispatcher_start(config or _dispatcher_config_path())


def cmd_setup(args):
    """First run: collect keys into .env, then offer to start the stack."""
    _hdr("Triad setup")
    if not CAIRN_DIR.is_dir():
        _warn(f"no Cairn checkout at {CAIRN_DIR}; run ./install.sh first")
        _warn("setup can still write .env, but the stack will not start without it")

    current = _env_read()
    interactive = sys.stdin.isatty() and not getattr(args, "yes", False)
    if not interactive and not getattr(args, "yes", False):
        _warn("stdin is not a terminal, so defaults are used and nothing is prompted")

    updates = {}
    chosen = None
    worker_target = None
    if interactive:
        # One question, both layers: asked separately it was two prompts, two keys, and two sides free
        # to disagree. `triad configure` covers the case where they should differ.
        chosen = _choose(
            "Which provider should Strix and the Cairn worker use?\n"
            "  (for a different model on either side, run:  triad configure)",
            PROVIDER_CHOICES, default=_default_provider(current))
        if isinstance(chosen, _Cancelled):
            return _aborted()

        # The same key serves both sides for these providers, so ask once. An existing
        # key is offered as the default so re-running setup does not demand it again.
        existing = current.get(chosen["key_var"]) or current.get("LLM_API_KEY")
        hint = f" [keep {_mask(existing)}]" if existing else ""
        key = _ask_secret(f"API key for {chosen['name']}{hint}")
        if isinstance(key, _Cancelled):
            return _aborted()
        if not key and existing:
            key = existing
            _ok(f"keeping the key already in {_env_path().name}")
        elif not key:
            _warn(f"no key entered; Strix and the worker will fail until "
                  f"{chosen['key_var']} is set")

        if key:
            updates["LLM_API_KEY"] = key
            updates[chosen["key_var"]] = key
            for var in chosen["also"]:
                updates[var] = key

        # With the key in hand, show what this provider actually serves instead of
        # asking for a model id from memory. One choice still drives both layers.
        raw = _pick_model(chosen, key, "Strix and the Cairn worker")
        if isinstance(raw, _Cancelled):
            return _aborted()
        worker_target = _worker_model_id(chosen, raw)
        updates["STRIX_LLM"] = _strix_model_id(chosen, raw)
        # Written empty rather than left out: a provider needing no custom endpoint has to clear the
        # previous one, or requests keep going to the old base url.
        updates["LLM_API_BASE"] = chosen["base"] or ""
        updates["LLM_EXTRA_HEADERS"] = _strix_headers() if chosen["headers"] else ""

        bind = _choose("Where should the Cairn API and console bind?", BIND_CHOICES, default=1)
        if isinstance(bind, _Cancelled):
            return _aborted()
        if bind:
            updates["CAIRN_BIND"] = bind
    else:
        fallback = PROVIDER_CHOICES[_default_provider(current) - 1][1]
        chosen = fallback
        worker_target = chosen["worker_model"]
        if current.get("STRIX_LLM"):
            updates["STRIX_LLM"] = current["STRIX_LLM"]
        else:
            # Take the whole provider, not just its model id: an openai/ model without
            # its endpoint (and the headers that endpoint wants) does not work.
            updates["STRIX_LLM"] = chosen["strix_llm"]
            updates["LLM_API_BASE"] = chosen["base"] or ""
            updates["LLM_EXTRA_HEADERS"] = _strix_headers() if chosen["headers"] else ""

    updates.setdefault("CAIRN_BASE_URL", current.get("CAIRN_BASE_URL") or DEFAULT_BASE)
    updates.setdefault("TRIAD_WORKDIR", current.get("TRIAD_WORKDIR") or strix.DEFAULT_WORKDIR)
    updates.setdefault("STRIX_TELEMETRY", current.get("STRIX_TELEMETRY") or "0")

    path = _env_write(updates)
    _hdr("Written")
    _ok(f"{path} (mode 600)")
    merged = _env_read()
    _ok(f"Strix model      {merged.get('STRIX_LLM') or '(unset)'}")
    if merged.get("LLM_API_BASE"):
        _ok(f"Strix endpoint   {merged['LLM_API_BASE']}")
    _ok(f"Strix key        {_mask(merged.get('LLM_API_KEY'))}")
    for name in ("OPENCODE_GO_API_KEY", "OPENROUTER_API_KEY", "DEEPSEEK_API_KEY",
                 "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "OPENAI_API_KEY"):
        if merged.get(name):
            _ok(f"Worker key       {name} {_mask(merged[name])}")

    # The worker half of the same choice. An unattended run must not undo a deliberate one: if
    # `triad configure` set the worker model, leave it alone and say so.
    keep_worker = (not interactive) and LOCAL_OVERRIDE.is_file()
    if keep_worker:
        _ok(f"Worker model     {_read_worker_model()} (kept; set by triad configure)")
    else:
        model_path, problem = _write_worker_model(worker_target)
        if problem:
            _warn(f"worker model not set: {problem}")
            _warn("the dispatcher will use whatever dispatch.local.yaml specifies")
        else:
            _ok(f"Worker model     {worker_target}")

    # opencode reads its own credentials file, so the key above goes straight in.
    # This is what makes a separate interactive `opencode auth login` unnecessary.
    worker_entries = {provider: merged[var]
                      for var, provider in OPENCODE_AUTH_KEYS.items() if merged.get(var)}
    if worker_entries:
        auth_path = _opencode_auth_set(worker_entries)
        for provider in sorted(worker_entries):
            _ok(f"Worker creds     {provider} -> {auth_path}")
    _ok(f"Cairn bind       {merged.get('CAIRN_BIND') or '(compose default)'}")

    if interactive:
        print("\n  Both layers use the provider above. For a different provider or model")
        print("  on either side, including a cheap worker and a stronger Strix model:")
        print("    triad configure")
        print()
        answer = _ask("Start Cairn and the dispatcher now? (Y/n)", "y")
        if isinstance(answer, _Cancelled):
            return _aborted()
        if answer.lower() not in ("n", "no"):
            return cmd_up(args)
    return 0


def _provider_for_model(model, field="strix_llm"):
    """Which provider entry a model id belongs to, if any."""
    for provider in PROVIDERS:
        if provider[field] == model:
            return provider
    return None


def _opencode_auth_for(key_var):
    """The opencode provider a key variable is written to, if it is one opencode reads."""
    return OPENCODE_AUTH_KEYS.get(key_var)


def _configure_show(current):
    """Report what each layer is set to, changing nothing."""
    _ok(f"Strix (discovery)      {current.get('STRIX_LLM') or '(unset)'}")
    if current.get("LLM_API_BASE"):
        _ok(f"                       endpoint {current['LLM_API_BASE']}")
    if current.get("LLM_EXTRA_HEADERS"):
        _ok("                       custom headers set (required by that endpoint)")
    _ok(f"                       key {_mask(current.get('LLM_API_KEY'))}")
    _ok(f"Worker (exploitation)  {_read_worker_model() or '(unset)'}")
    _ok(f"                       config {_dispatcher_config_path()}")
    for var, provider in OPENCODE_AUTH_KEYS.items():
        if current.get(var):
            _ok(f"                       key {var} {_mask(current[var])} -> {provider}")
    for var in ("DEEPSEEK_API_KEY", "ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN",
                "OPENAI_API_KEY"):
        if current.get(var):
            _ok(f"                       key {var} {_mask(current[var])}")
    return 0


def cmd_configure(args):
    """Give Strix and the Cairn worker their own provider and model.

    `triad setup` picks one provider for both, which is what most installs want. This
    is the escape hatch for when they should differ: a cheap model on the high-volume
    worker and a stronger one on Strix, or a worker on a subscription while Strix uses
    a billed API.
    """
    _hdr("Model providers")
    current = _env_read()
    if args.show:
        return _configure_show(current)

    env_changes, worker_model, worker_key = {}, None, None
    worker_key_var = None
    from_flags = bool(args.strix_model or args.strix_key or args.worker_provider
                      or args.worker_model or args.worker_key)
    if not from_flags and not sys.stdin.isatty():
        _configure_show(current)
        _warn("stdin is not a terminal and no flags were given, so nothing changed")
        print("     set one side:  triad configure --strix-model openrouter/z-ai/glm-5.3")
        return 0

    given = {}                       # key var -> value, so one key is not asked twice

    def key_for(var, name):
        if var in given:
            return given[var]
        existing = current.get(var)
        hint = f" [keep {_mask(existing)}]" if existing else ""
        value = _ask_secret(f"API key for {name}{hint}")
        if isinstance(value, _Cancelled):
            return _Cancelled
        value = value or existing or ""
        given[var] = value
        return value

    if from_flags:
        if args.strix_model:
            provider = _provider_for_model(args.strix_model)
            env_changes["STRIX_LLM"] = args.strix_model
            env_changes["LLM_API_BASE"] = (provider or {}).get("base") or ""
            env_changes["LLM_EXTRA_HEADERS"] = (
                _strix_headers() if (provider or {}).get("headers") else "")
            if provider is None:
                _warn(f"{args.strix_model} is not one of the known providers, so no")
                _warn("endpoint is set; an openai/ model needs LLM_API_BASE as well")
        if args.strix_key:
            env_changes["LLM_API_KEY"] = _clean(args.strix_key)
        if args.worker_provider:
            provider = _provider_named(args.worker_provider)
            if provider is None:
                _err(f"unknown provider {args.worker_provider!r}")
                print("     known: " + ", ".join(p["name"] for p in PROVIDERS))
                return 2
            worker_model, worker_key_var = provider["worker_model"], provider["key_var"]
        if args.worker_model:
            worker_model = args.worker_model
            # Infer the provider from the model when it is a known one, so the key
            # below lands in the right variable instead of a guessed one.
            inferred = _provider_for_model(worker_model, "worker_model")
            if inferred:
                worker_key_var = inferred["key_var"]
        if args.worker_key:
            worker_key = _clean(args.worker_key)
            if not worker_key_var:
                _err("a worker key needs to know which provider holds it")
                print("     pass --worker-provider, or use a known model id so it can")
                print("     be inferred:  " + ", ".join(p["worker_model"] for p in PROVIDERS))
                return 2
            env_changes[worker_key_var] = worker_key
    else:
        strix_choices: list = PROVIDER_CHOICES + [
            ("Other: type a LiteLLM model id", OTHER),
            ("Keep the current Strix model", KEEP),
        ]
        pick = _choose("Which model should Strix (discovery) drive?", strix_choices,
                       default=_default_provider(current))
        if isinstance(pick, _Cancelled):
            return _aborted()
        if pick is OTHER:
            typed = _ask("Strix model id (LiteLLM form, e.g. openrouter/z-ai/glm-5.3)")
            if isinstance(typed, _Cancelled):
                return _aborted()
            # An arbitrary id cannot imply an endpoint, so any previous one is cleared.
            env_changes["STRIX_LLM"] = typed
            env_changes["LLM_API_BASE"] = ""
            env_changes["LLM_EXTRA_HEADERS"] = ""
        elif pick is not KEEP:
            # Key first, because the model list is fetched with it.
            key = key_for(pick["key_var"], pick["name"])
            if isinstance(key, _Cancelled):
                return _aborted()
            raw = _pick_model(pick, key, "Strix (discovery)")
            if isinstance(raw, _Cancelled):
                return _aborted()
            env_changes["STRIX_LLM"] = _strix_model_id(pick, raw)
            env_changes["LLM_API_BASE"] = pick["base"] or ""
            env_changes["LLM_EXTRA_HEADERS"] = _strix_headers() if pick["headers"] else ""
            if key:
                env_changes["LLM_API_KEY"] = key
                # Record the key under the provider's own variable as well, as setup does: that is where it
                # belongs in .env, and without it a later run cannot tell the provider is already configured.
                if pick["key_var"] != "LLM_API_KEY":
                    env_changes[pick["key_var"]] = key
                for var in pick["also"]:
                    env_changes[var] = key
                # The same key usually drives the worker too, so hand it over rather
                # than asking for it a second time.
                given[pick["key_var"]] = key

        worker_choices: list = PROVIDER_CHOICES + [
            ("Other: type an opencode model id", OTHER),
            ("Keep the current worker model", KEEP),
        ]
        wpick = _choose("Which model should the Cairn worker (exploitation) use?",
                        worker_choices, default=_default_provider(current, "worker"))
        if isinstance(wpick, _Cancelled):
            return _aborted()
        if wpick is OTHER:
            typed = _ask("Worker model id (opencode form, e.g. openrouter/z-ai/glm-5.3)")
            if isinstance(typed, _Cancelled):
                return _aborted()
            worker_model = typed
        elif wpick is not KEEP:
            key = key_for(wpick["key_var"], wpick["name"])
            if isinstance(key, _Cancelled):
                return _aborted()
            raw = _pick_model(wpick, key, "the Cairn worker")
            if isinstance(raw, _Cancelled):
                return _aborted()
            worker_model = _worker_model_id(wpick, raw)
            if key:
                env_changes[wpick["key_var"]] = key
                worker_key_var, worker_key = wpick["key_var"], key

    _hdr("Applied")
    if env_changes:
        env_path = _env_write(env_changes)
        _ok(f"{env_path} (mode 600)")
    if worker_model:
        model_path, problem = _write_worker_model(worker_model)
        if problem:
            _err(problem)
            return 2
        _ok(f"worker model  {worker_model}")
        _ok(f"              {model_path}")
    if worker_key and _opencode_auth_for(worker_key_var):
        auth = _opencode_auth_set({_opencode_auth_for(worker_key_var): worker_key})
        _ok(f"worker creds  {_opencode_auth_for(worker_key_var)} -> {auth}")
    if not (env_changes or worker_model):
        _ok("nothing changed")
    else:
        _configure_show(_env_read())
        print("\n  Restart for a worker change to take effect:  triad up")
    return 0


def cmd_models(args):
    """List the models a provider serves, so no model id has to be guessed.

    The wizards show this list when they ask for a model; this is the same list from
    the command line, for looking before configuring or for scripting.
    """
    _hdr("Available models")
    current = _env_read()
    provider = _provider_named(args.provider) if args.provider else None
    if provider is None:
        provider = (_provider_for_model(current.get("STRIX_LLM") or "")
                    or PROVIDER_CHOICES[_default_provider(current) - 1][1])
    key = current.get(provider["key_var"]) or current.get("LLM_API_KEY")
    ids, problem = _fetch_models(provider, key)
    if problem:
        _err(f"{provider['name']}: {problem}")
        if provider.get("auth") != "none" and not key:
            print(f"     no {provider['key_var']} in {_env_path().name} to list with")
        print("     known providers: " + ", ".join(p["name"] for p in PROVIDERS))
        return 2
    matches = [i for i in ids if args.filter.lower() in i.lower()] if args.filter else ids
    _ok(f"{provider['name']}: {len(matches)} of {len(ids)} models")
    for model in matches[:args.limit]:
        print(f"    {model}")
    if len(matches) > args.limit:
        print(f"    ... and {len(matches) - args.limit} more (--limit, or --filter)")
    if matches:
        print(f"\n  Strix uses  {_strix_model_id(provider, matches[0])}")
        print(f"  the worker  {_worker_model_id(provider, matches[0])}")
        print("  setup and configure write both forms for whichever model you pick")
    return 0


def cmd_up(args):
    """Bring the stack up: Cairn server, then the dispatcher."""
    _hdr("Starting the stack")
    if not CAIRN_DIR.is_dir():
        _err(f"no Cairn checkout at {CAIRN_DIR}; run ./install.sh first")
        return 2

    # Both routes need something: Docker for the server, uv for the host server
    # and for the dispatcher either way. Say which is missing before trying.
    state = _docker_state()
    if not _cairn_up() and state != "ok" and _uv_bin() is None:
        _err("cannot start the stack: no usable Docker and no uv")
        print("     ./install.sh installs both, then re-run: triad up")
        print("     uv alone is enough (it runs Cairn as a host process):")
        print("       curl -LsSf https://astral.sh/uv/install.sh | sh")
        return 2

    if _cairn_up():
        _ok(f"Cairn already answering at {_base_url()}")
    elif getattr(args, "container", False):
        if state != "ok":
            reason, fix = _docker_remedy(state)
            _err(f"--container needs a usable Docker: {reason}")
            print(f"     fix: {fix}")
            return 2
        compose = _compose_cmd()
        if compose is None:                      # unreachable when state is ok
            _err("docker compose is not available")
            return 2
        if platform.machine().lower() not in ("x86_64", "amd64"):
            _warn(f"the worker image is amd64-only and this host is {platform.machine()}")
            _warn("worker containers may fail to start; local mode is the default here")
        _ok("building and starting cairn-server (first run builds the image)")
        if subprocess.run(compose + ["up", "-d", "--build", "cairn-server"],
                          cwd=str(REPO)).returncode != 0:
            _err("docker compose failed; see the output above")
            return 2
        if not _wait_for_cairn():
            _err(f"Cairn did not come up; try: {SERVER_LOG} or docker compose logs cairn-server")
            return 2
    elif state == "ok":
        compose = _compose_cmd()
        if compose is None:
            _err("docker compose is not available")
            return 2
        _ok("starting cairn-server with docker compose (first run builds the image)")
        if subprocess.run(compose + ["up", "-d", "--build", "cairn-server"],
                          cwd=str(REPO)).returncode != 0:
            _err("docker compose failed; see the output above")
            return 2
        if not _wait_for_cairn():
            _err("Cairn did not come up; check: docker compose logs cairn-server")
            return 2
    else:
        # Name the actual Docker problem and how to clear it. "Docker is not usable"
        # on its own is what sent someone to reinstalling an already-working Docker.
        reason, fix = _docker_remedy(state)
        _warn(f"Docker is not usable here: {reason}")
        print(f"     fix: {fix}")
        _warn("falling back to a host Cairn server (no-sandbox mode)")
        pid, problem = _server_local_start()
        if problem:
            _err(problem)
            _print_tail(SERVER_LOG)
            return 2
        _ok(f"cairn serve started (pid {pid}, log {SERVER_LOG})")
        if not _wait_for_cairn(timeout=90):
            _err("Cairn did not answer; last lines of its log:")
            _print_tail(SERVER_LOG)
            return 2

    pid = _pid_alive(DISPATCH_PID)
    if pid:
        _ok(f"the dispatcher is already running (pid {pid})")
    elif getattr(args, "container", False):
        _warn("--container start the server only; the compose dispatcher needs api keys")
        _warn("in dispatch.yaml, and an amd64 worker image (use local mode otherwise)")
    else:
        worker, where = _worker_cli()
        if worker is None:
            _warn("no worker CLI found (opencode, claude, codex or pi)")
            print("     the dispatcher needs one to claim intents, so nothing will move")
            print("     install one:   ./install.sh          (installs opencode)")
            print("     then the key:  triad setup            (writes opencode's credentials)")
        else:
            _ok(f"worker CLI: {worker} ({where})")
            if worker == "opencode" and not _opencode_credentials():
                _warn("opencode has no credentials yet, so the worker will fail on its")
                _warn("first model call. Put the key in its auth file with:")
                print("       triad auth        (reads OPENCODE_GO_API_KEY from .env)")
        config = _dispatcher_config_path(getattr(args, "config", None))
        pid, problem = _dispatcher_start(config)
        if problem:
            _err(f"the dispatcher did not start: {problem}")
            _print_tail(DISPATCH_LOG)
            return 2
        _ok(f"dispatcher started (pid {pid}, config {config}, log {DISPATCH_LOG})")

    _hdr("Ready")
    _ok(f"Cairn API   {_base_url()}")
    _ok(f"console     {_base_url()}  (open in a browser)")
    print()
    print("  Next: run an engagement")
    print('    triad engage --title ACME --target https://app.example \\')
    print('                 --goal "conclude or rule out every finding in scope" \\')
    print("                 --roe contracts/roe-instructions.md")
    print("    triad status")
    return 0


def _docker_containers():
    """Names of the triad containers Docker knows about, running or stopped."""
    try:
        probe = subprocess.run(["docker", "ps", "-a", "--filter", "name=triad-cairn",
                                "--format", "{{.Names}}"],
                               capture_output=True, text=True, timeout=30)
    except (OSError, subprocess.SubprocessError):
        return None
    if probe.returncode != 0:
        return None
    return [line.strip() for line in probe.stdout.splitlines() if line.strip()]


def _teardown_containers(keep):
    """Remove (or stop) the compose containers, reporting what actually happened.

    Returns 0 when they are down or were already gone, 2 when Docker is unusable or
    compose fails. Output is captured so a failure is not mistaken for a success.
    """
    state = _docker_state()
    compose = _compose_cmd() if state == "ok" else None
    if state != "ok" or compose is None:
        if state != "ok":
            reason, fix = _docker_remedy(state)
        else:                      # unreachable when state is ok; never claim success
            reason, fix = "docker compose is not available", "./install.sh provides it"
        _warn(f"containers left alone: {reason}")
        print(f"     fix: {fix}")
        return 2

    names = _docker_containers()
    if names is None:
        _err("could not list the containers with 'docker ps'")
        return 2
    if not names:
        _ok("the containers were not running")
        return 0

    verb = "stop" if keep else "down"
    argv = ["stop", "cairn-server", "cairn-dispatcher"] if keep else ["down"]
    try:
        result = subprocess.run(compose + argv, cwd=str(REPO), capture_output=True,
                                text=True, timeout=180)
    except (OSError, subprocess.SubprocessError) as exc:
        _err(f"docker compose {verb} could not run: {exc}")
        return 2
    if result.returncode != 0:
        _err(f"docker compose {verb} failed (rc {result.returncode})")
        for line in [l for l in (result.stderr or "").splitlines() if l.strip()][-8:]:
            print(f"     | {line}")
        print("     hint: inspect them with 'docker compose ps' in the repo, then retry")
        return 2

    if keep:
        _ok(f"containers stopped, kept in place ({', '.join(names)})")
    else:
        _ok(f"containers and network removed ({', '.join(names)})")
    return 0


def cmd_down(args):
    """Stop the dispatcher and the Cairn server and take the containers down."""
    _hdr("Stopping the stack")
    if _stop_pid(DISPATCH_PID, "the dispatcher"):
        _ok("dispatcher stopped")
    else:
        _ok("the dispatcher was not running")
    if getattr(args, "keep_server", False):
        _ok("leaving the Cairn server up (--keep-server)")
        return 0
    if _stop_pid(SERVER_PID, "the host cairn serve"):
        _ok("host Cairn server stopped")
    code = _teardown_containers(getattr(args, "keep_containers", False))
    _ok("data kept in ./datas/cairn; it survives the down")
    return code


def _opencode_auth_path():
    """Where opencode keeps credentials.

    It follows XDG_DATA_HOME like the rest of the CLI, so honour that instead of
    hardcoding ~/.local/share.
    """
    base = os.environ.get("XDG_DATA_HOME") or (Path.home() / ".local" / "share")
    return Path(base) / "opencode" / "auth.json"


# .env variable -> opencode auth provider name. Writing the file this way is what
# makes `opencode auth login` unnecessary: the CLI reads it directly.
OPENCODE_AUTH_KEYS = {
    "OPENCODE_GO_API_KEY": "opencode-go",
    "OPENROUTER_API_KEY": "openrouter",
}


def _opencode_auth_set(entries):
    """Merge {provider: key} into opencode's auth.json, leaving other providers be.

    The file holds keys, so it is chmod 600. An unreadable or malformed file is
    replaced rather than merged, since a partial JSON parse would lose credentials
    the user still has.
    """
    path = _opencode_auth_path()
    data = {}
    if path.is_file():
        try:
            existing = json.loads(path.read_text(encoding="utf-8") or "{}")
            if isinstance(existing, dict):
                data = existing
        except (ValueError, OSError):
            data = {}
    for provider, key in entries.items():
        data[provider] = {"type": "api", "key": key}
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
    path.chmod(0o600)
    return path


def _opencode_credentials():
    """Provider names opencode currently has a usable api key for."""
    path = _opencode_auth_path()
    if not path.is_file():
        return []
    try:
        data = json.loads(path.read_text(encoding="utf-8") or "{}")
    except (ValueError, OSError):
        return []
    if not isinstance(data, dict):
        return []
    return sorted(name for name, entry in data.items()
                  if isinstance(entry, dict) and entry.get("key"))


def cmd_auth(args):
    """Write the worker CLI's credentials from .env.

    Removes the interactive step: opencode reads its own auth file, so the key can
    be placed there directly instead of asking the user to run `auth login`.
    """
    _hdr("Worker credentials")
    env = _env_read()
    if getattr(args, "show", False):
        providers = _opencode_credentials()
        if providers:
            _ok(f"{_opencode_auth_path()}")
            for name in providers:
                _ok(f"  {name}: configured")
        else:
            _warn(f"no credentials in {_opencode_auth_path()}")
            print("     add one with:  triad auth        (reads OPENCODE_GO_API_KEY)")
        return 0

    key = getattr(args, "key", None)
    if key:
        # Same cleaning as the wizard: a key pasted from a terminal that mangles
        # control keys must not carry those bytes into opencode's credentials.
        entries = {getattr(args, "provider", None) or "opencode-go": _clean(key)}
    else:
        entries = {provider: env[var]
                   for var, provider in OPENCODE_AUTH_KEYS.items() if env.get(var)}
    if not entries:
        _warn(f"no worker key found in {_env_path()}")
        print("     set one:  triad setup        (or add OPENCODE_GO_API_KEY to .env)")
        return 2
    path = _opencode_auth_set(entries)
    for provider in sorted(entries):
        _ok(f"{provider} -> {path}")
    _ok("opencode uses this directly; no interactive login needed")
    return 0


def _dashboard_available():
    """Whether the dashboard could open: node >= 18 and its deps are on disk.

    cmd_home uses this to fall back silently; cmd_tui still owns the message
    that names what is missing for an explicit `triad tui`.
    """
    tui = _repo_root() / "tui"
    if not (tui / "node_modules" / "ink").is_dir():
        return False
    node = _node_bin()
    if not node:
        return False
    major = _node_major(_node_version(node))
    return major is not None and major >= 18


def _on_tty():
    return sys.stdin.isatty() and sys.stdout.isatty()


def cmd_home(args):
    """Bare `triad`: open the dashboard, or say where things stand when it cannot."""
    env = _env_read()
    configured = bool(env.get("LLM_API_KEY"))
    # The escape hatches keep the scriptable summary reachable on a terminal.
    text_only = getattr(args, "status", False) or getattr(args, "no_tui", False)
    if not configured:
        print(f"{_colour('triad', _BOLD)}  Strix (discovery) + Cairn (exploitation)")
        _warn("not configured yet" if env else f"no {_env_path()} yet")
        if sys.stdin.isatty() and not text_only:
            print("  Running the setup wizard.")
            code = cmd_setup(args)
            # A first run should land in the dashboard, not the summary, when it can.
            if code == 0 and _on_tty() and _dashboard_available():
                return cmd_tui(args)
            return code
        print("  Run:  triad setup")
        return 0

    if not text_only and _on_tty() and _dashboard_available():
        return cmd_tui(args)

    print(f"{_colour('triad', _BOLD)}  Strix (discovery) + Cairn (exploitation)")
    _ok(f"model {env.get('STRIX_LLM') or '(unset)'}")
    _ok(f"key   {_mask(env.get('LLM_API_KEY'))}")
    base = _base_url()
    if _cairn_up():
        _ok(f"Cairn answering at {base}")
    else:
        _warn(f"Cairn is not answering at {base}")
        state = _docker_state()
        if state != "ok":
            reason, _fix = _docker_remedy(state)
            _warn(f"docker: {reason}")
    pid = _pid_alive(DISPATCH_PID)
    if pid:
        _ok(f"dispatcher running (pid {pid}, log {DISPATCH_LOG})")
    else:
        _warn("dispatcher not running (no projects will move without it)")
    if (_repo_root() / "tui" / "node_modules" / "ink").is_dir():
        _ok("dashboard ready (triad tui)")
    if not _cairn_up() or pid is None:
        print("  Start the stack:  triad up")
    try:
        projects = client().list_projects()
    except Exception:
        projects = []
    if projects:
        _hdr("Engagements")
        for project in projects[:8]:
            print(f"  {project.get('id','?'):10} {project.get('status','?'):9} "
                  f"{project.get('title','')}")
        print("  Inspect one:  triad status --project <id> --path")
    else:
        print("  No engagements yet:  triad engage --help")
    return 0


def main(argv=None):
    ap = argparse.ArgumentParser(prog="triad", description=(__doc__ or "triad").splitlines()[0])
    # Bare `triad` opens the dashboard; these two keep the text summary for scripts
    # and for a terminal that should not host the TUI.
    ap.add_argument("--status", action="store_true",
                    help="print the text summary instead of opening the dashboard")
    ap.add_argument("--no-tui", action="store_true",
                    help="print the text summary instead of opening the dashboard")
    sub = ap.add_subparsers(dest="cmd")

    su = sub.add_parser("setup", help="interactive first run: keys, then start the stack")
    su.add_argument("--yes", action="store_true", help="take every default, prompt nothing")
    su.set_defaults(func=cmd_setup)

    up = sub.add_parser("up", help="start the Cairn server and the dispatcher")
    up.add_argument("--config", help="dispatcher config (default: dispatch.local.yaml)")
    up.add_argument("--container", action="store_true",
                    help="use the compose dispatcher instead of the host one (amd64 only)")
    up.set_defaults(func=cmd_up)

    mo = sub.add_parser("models", help="list the models a provider serves")
    mo.add_argument("--provider", help="provider name (default: whatever Strix is set to)")
    mo.add_argument("--filter", help="only ids containing this text")
    mo.add_argument("--limit", type=int, default=30, help="how many to print (default 30)")
    mo.set_defaults(func=cmd_models)

    cf = sub.add_parser("configure",
                        help="set the provider and model for Strix and the Cairn worker")
    cf.add_argument("--show", action="store_true", help="report both sides, change nothing")
    cf.add_argument("--strix-model", help="LiteLLM model id for Strix")
    cf.add_argument("--strix-key", help="API key for Strix's provider")
    cf.add_argument("--worker-provider", help="provider name for the Cairn worker")
    cf.add_argument("--worker-model", help="opencode model id for the Cairn worker")
    cf.add_argument("--worker-key", help="API key for the worker's provider")
    cf.set_defaults(func=cmd_configure)

    dn = sub.add_parser("down", help="stop the stack and remove the containers")
    dn.add_argument("--keep-server", action="store_true",
                    help="stop only the dispatcher, leave the Cairn server up")
    dn.add_argument("--keep-containers", action="store_true",
                    help="stop the containers but do not remove them")
    dn.set_defaults(func=cmd_down)

    au = sub.add_parser("auth", help="write the worker CLI's credentials from .env")
    au.add_argument("--key", help="key to write (default: read OPENCODE_GO_API_KEY from .env)")
    au.add_argument("--provider", help="opencode provider name (default: opencode-go)")
    au.add_argument("--show", action="store_true", help="report what is configured, write nothing")
    au.set_defaults(func=cmd_auth)

    e = sub.add_parser("engage", help="run the default flow: project, Strix scan, feed the graph")
    e.add_argument("--title", required=True)
    e.add_argument("--target", required=True)
    e.add_argument("--goal", required=True)
    e.add_argument("--roe")
    e.add_argument("--workdir", help="where the Strix run goes (default: $TRIAD_WORKDIR/<title>)")
    e.add_argument("--mode", default="quick", choices=["quick", "standard", "deep"])
    e.add_argument("--max-turns", type=int, default=60)
    e.add_argument("--scan-timeout", type=int, default=3600,
                   help="how long to wait for the scan to settle (default 3600s)")
    e.add_argument("--anchor", default="origin", help="graph fact the findings hang off")
    e.add_argument("--no-scan", action="store_true",
                   help="only create the project; do not scan or feed")
    e.add_argument("--no-pause", action="store_true",
                   help="leave the dispatcher running while the scan runs (allows overlap)")
    e.add_argument("--hold", action="store_true",
                   help="do not start Cairn on the graph after feeding")
    e.add_argument("--config", help="dispatcher config to start Cairn with (default dispatch.local.yaml)")
    e.add_argument("--watch", action="store_true", help="follow the graph once fed")
    e.add_argument("--watch-timeout", type=int, default=1800)
    e.add_argument("--interval", type=int, default=15)
    e.add_argument("--no-bootstrap", action="store_true")
    e.add_argument("--json", action="store_true")
    e.set_defaults(func=cmd_engage)

    s = sub.add_parser("scan", help="launch a headless Strix scan")
    s.add_argument("--target", required=True)
    s.add_argument("--workdir", required=True)
    s.add_argument("--roe")
    s.add_argument("--mode", default="quick", choices=["quick", "standard", "deep"])
    s.add_argument("--max-turns", type=int, default=60)
    s.add_argument("--wait", action="store_true")
    s.add_argument("--wait-timeout", type=int, default=3600)
    s.set_defaults(func=cmd_scan)

    f = sub.add_parser("findings", help="read a Strix run")
    f.add_argument("--workdir", required=True)
    f.add_argument("--run")
    f.set_defaults(func=cmd_findings)

    pg = sub.add_parser("progress", help="how far along a running Strix scan is")
    pg.add_argument("--workdir", help="engagement directory holding strix_runs/")
    pg.add_argument("--run", help="run name (default: the most recent)")
    pg.add_argument("-f", "--follow", action="store_true",
                    help="keep printing until the run stops")
    pg.add_argument("--interval", type=int, default=15, help="seconds between updates")
    pg.add_argument("--verbose", action="store_true",
                    help="include the agent stream, agents, todos, findings, coverage and log")
    pg.add_argument("--log-lines", type=int, default=200,
                    help="lines of strix.log to include with --verbose (default 200)")
    pg.add_argument("--messages", type=int, default=200,
                    help="most recent agent messages to include with --verbose (default 200)")
    pg.add_argument("--json", action="store_true")
    pg.set_defaults(func=cmd_progress)

    vw = sub.add_parser("view", help="open Strix's live dashboard for a run")
    vw.add_argument("--workdir", help="engagement directory holding strix_runs/")
    vw.add_argument("--run", help="run name (default: the most recent)")
    vw.add_argument("--port", type=int)
    vw.add_argument("--host", help="0.0.0.0 to expose it beyond localhost")
    vw.add_argument("--no-open", action="store_true", help="do not open a browser")
    vw.set_defaults(func=cmd_view)

    rn = sub.add_parser("runs", help="every run triad knows about, newest first")
    rn.add_argument("--json", action="store_true", help="the dashboard's data, verbatim")
    rn.set_defaults(func=cmd_runs)

    ct = sub.add_parser("control", help="pause, resume, stop or delete a scan or a project")
    ct.add_argument("action", choices=["pause", "resume", "stop", "delete"])
    ct.add_argument("--workdir", help="the engagement directory of the scan")
    ct.add_argument("--run", help="run name, when deleting a run")
    ct.add_argument("--project", help="a Cairn project id, instead of a scan")
    ct.set_defaults(func=cmd_control)

    tu = sub.add_parser("tui", help="open the dashboard: runs, progress, telemetry, controls")
    tu.add_argument("--interval", type=float, help="seconds between refreshes")
    tu.set_defaults(func=cmd_tui)

    fd = sub.add_parser("feed", help="post a Strix run into the Cairn graph")
    fd.add_argument("--project", required=True)
    fd.add_argument("--workdir", required=True)
    fd.add_argument("--run")
    fd.add_argument("--anchor", default="origin")
    fd.set_defaults(func=cmd_feed)

    w = sub.add_parser("watch", help="poll the graph until completed/stopped/timeout")
    w.add_argument("--project", required=True)
    w.add_argument("--timeout", type=int, default=1800)
    w.add_argument("--interval", type=int, default=15)
    w.set_defaults(func=cmd_watch)

    st = sub.add_parser("status", help="list projects or show one graph")
    st.add_argument("--project")
    st.add_argument("--path", action="store_true", help="also print the attack path")
    st.set_defaults(func=cmd_status)

    r = sub.add_parser("report", help="render a markdown report from the graph")
    r.add_argument("--project", required=True)
    r.add_argument("--workdir")
    r.add_argument("--run")
    r.add_argument("-o", "--output")
    r.set_defaults(func=cmd_report)

    # Bare `triad` shows where things stand and offers setup when it is needed.
    ap.set_defaults(func=cmd_home)

    args = ap.parse_args(argv)
    # .env is what the wizard writes; without this the CLI would ignore it, since
    # only docker compose reads that file on its own.
    _load_dotenv()
    try:
        return args.func(args)
    except cairn.CairnError as exc:
        base = os.environ.get("CAIRN_BASE_URL", cairn.DEFAULT_BASE)
        if exc.status is None:
            print(f"error: Cairn is not answering at {base}\n       {exc}", file=sys.stderr)
            print("       start it with:  triad up", file=sys.stderr)
        else:
            print(f"error: Cairn returned {exc.status}: {exc}", file=sys.stderr)
        return 2
    except FileNotFoundError as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("\ninterrupted", file=sys.stderr)
        return 130


if __name__ == "__main__":
    raise SystemExit(main())
