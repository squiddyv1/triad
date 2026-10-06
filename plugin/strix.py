"""Strix orchestration helpers: run headless scans, normalise findings.

Grounded on strix 1.6.2 (verified locally):
  * headless flag is `-n`; exit codes 0 = clean, 1 = fatal, 2 = vulns found
  * `--max-turns` is the reliable guardrail; `--max-budget` silently no-ops on
    models LiteLLM cannot price (custom OpenAI-compatible endpoints).
  * artifacts land in strix_runs/<run-name>/, canonical set:
      run.json, vulnerabilities.json, vulnerabilities/*.md,
      penetration_test_report.md, findings.sarif, coverage.json, .state/
  * findings.sarif also contains *coverage gaps* (ruleId prefix "strix-coverage/",
    kind "open", level "none") -- never count those as vulnerabilities.
    `vulnerabilities.json` is the clean machine-readable list.
"""

from __future__ import annotations

import json
import os
import shutil
import sqlite3
import subprocess
from pathlib import Path

COVERAGE_RULE_PREFIX = "strix-coverage/"

# Where a launched scan's pid is recorded, so it can be paused or stopped later.
PID_FILE = "strix_last_launch.pid"

# Where scans, reports and engagement artifacts live when nothing overrides it.
DEFAULT_WORKDIR = "~/engagements"

# Label Cairn records for leads this stack posts, so the graph shows where a lead came
# from. Not "hermes": the driver posts these, and Hermes is optional.
FEED_CREATOR = "triad.strix"


def _resolve_bin() -> str:
    found = shutil.which("strix")
    if found:
        return found
    home_bin = Path.home() / ".strix" / "bin" / "strix"
    return str(home_bin) if home_bin.exists() else "strix"


def available() -> bool:
    """Whether a Strix binary can actually be run, wherever it was installed.

    Single source of truth for the install layout: the CLI asked this question with its
    own copy of the ~/.strix/bin guess, which would drift from this one.
    """
    return bool(shutil.which("strix")) or (Path.home() / ".strix" / "bin" / "strix").exists()


def post_leads(client, project_id, run, anchor="origin"):
    """Post a run's leads into the graph. Returns (hint_ids, intent_ids).

    Both front ends (the CLI and the Hermes tools) feed the same graph, so the posting
    lives here once instead of being copied into each of them.
    """
    hints, intents = to_cairn_leads(run)
    hints_posted = [client.add_hint(project_id, f"[strix] {h}", FEED_CREATOR).get("id")
                    for h in hints]
    intents_posted = [client.add_intent(project_id, [anchor], d, FEED_CREATOR).get("id")
                      for d in intents]
    return hints_posted, intents_posted


def run_scan(target, cwd, instruction_file=None, instruction=None, scan_mode="quick",
             max_turns=60, run_name=None, extra_targets=None, timeout=None) -> dict:
    """Launch a headless Strix scan. Returns immediately (Popen) with the run dir.

    Keep this non-blocking: a scan is tens of minutes. Poll with read_run().
    """
    cwd = Path(cwd).expanduser().resolve()
    cwd.mkdir(parents=True, exist_ok=True)
    cmd = [_resolve_bin(), "-n", "--target", str(target), "--scan-mode", scan_mode,
           "--max-turns", str(max_turns)]
    for t in (extra_targets or []):
        cmd += ["--target", str(t)]
    if instruction_file:
        cmd += ["--instruction-file", str(Path(instruction_file).expanduser().resolve())]
    if instruction:
        cmd += ["--instruction", instruction]

    log = cwd / "strix_last_launch.log"
    fh = open(log, "ab")
    env = dict(os.environ)
    env.setdefault("STRIX_TELEMETRY", "0")
    proc = subprocess.Popen(cmd, cwd=str(cwd), stdout=fh, stderr=subprocess.STDOUT,
                            start_new_session=True, env=env)
    # The pid is what makes a running scan controllable (pause, stop) after the fact:
    # nothing else on disk records which process belongs to which workdir.
    pid_file = cwd / PID_FILE
    pid_file.write_text(str(proc.pid), encoding="utf-8")
    return {"pid": proc.pid, "cwd": str(cwd), "cmd": cmd, "pid_file": str(pid_file),
            "log": str(log), "note": "poll strix_runs/ for artifacts"}


def read_pid(cwd) -> int | None:
    """The pid of the scan launched from this workdir, if one was recorded."""
    try:
        pid = int((Path(cwd).expanduser() / PID_FILE).read_text(encoding="utf-8").strip())
    except (OSError, ValueError):
        return None
    return pid if pid > 0 else None


def latest_run_dir(cwd) -> Path | None:
    runs = Path(cwd).expanduser().resolve() / "strix_runs"
    if not runs.is_dir():
        return None
    dirs = [d for d in runs.iterdir() if d.is_dir()]
    return max(dirs, key=lambda d: d.stat().st_mtime) if dirs else None


def resolve_run(cwd, run_name=None) -> Path:
    base = Path(cwd).expanduser().resolve() / "strix_runs"
    if run_name:
        p = base / run_name
        if not p.is_dir():
            raise FileNotFoundError(f"no such strix run: {p}")
        return p
    p = latest_run_dir(cwd)
    if not p:
        raise FileNotFoundError(f"no strix runs under {base}")
    return p


def read_run(cwd, run_name=None) -> dict:
    """Read a finished (or in-flight) run directory into a normalised summary."""
    run_dir = resolve_run(cwd, run_name)
    out = {"run_dir": str(run_dir), "run": run_dir.name,
           "findings": [], "coverage_gaps": [], "status": None,
           "cost_usd": None, "turns": None, "vulnerabilities_count": None}

    run_json = run_dir / "run.json"
    if run_json.is_file():
        try:
            meta = json.loads(run_json.read_text())
            out["status"] = meta.get("status")
            usage = meta.get("llm_usage") or {}
            out["cost_usd"] = usage.get("cost")
            out["turns"] = meta.get("turns") or meta.get("turn_count")
            for key in ("vulnerabilities", "findings", "results"):
                if isinstance(meta.get(key), list):
                    out["findings"] = [normalise(f) for f in meta[key]]
                    break
        except (json.JSONDecodeError, OSError):
            pass

    vj = run_dir / "vulnerabilities.json"
    if vj.is_file():
        try:
            data = json.loads(vj.read_text())
            if isinstance(data, dict):
                data = data.get("vulnerabilities") or data.get("findings") or []
            if isinstance(data, list) and data:
                out["findings"] = [normalise(f) for f in data]
                out["vulnerabilities_count"] = len(data)
        except (json.JSONDecodeError, OSError):
            pass

    # Only SARIF carries the coverage gaps. Its results are a fallback finding
    # source used when vulnerabilities.json is missing, or the same finding counts twice.
    sarif = run_dir / "findings.sarif"
    if sarif.is_file():
        try:
            doc = json.loads(sarif.read_text())
            sarif_findings = []
            for r in doc.get("runs", []):
                for res in r.get("results", []):
                    rule = (res.get("ruleId") or "")
                    entry = {
                        "rule": rule,
                        "level": res.get("level"),
                        "kind": res.get("kind"),
                        "message": (res.get("message") or {}).get("text", ""),
                        "location": _sarif_location(res),
                    }
                    if rule.startswith(COVERAGE_RULE_PREFIX) or res.get("kind") == "open":
                        out["coverage_gaps"].append(entry)
                    else:
                        sarif_findings.append(normalise({
                            "id": rule, "title": entry["message"] or rule,
                            "rule": rule, "severity": _level_to_severity(res.get("level")),
                            "target": entry["location"], "description": entry["message"],
                        }))
            if not out["findings"] and sarif_findings:
                out["findings"] = sarif_findings
        except (json.JSONDecodeError, OSError):
            pass

    if out["findings"] and out["vulnerabilities_count"] is None:
        out["vulnerabilities_count"] = len(out["findings"])
    return out


def _read_json(path, default):
    try:
        return json.loads(Path(path).read_text())
    except (OSError, ValueError):
        return default


def run_progress(cwd, run_name=None, _run=None) -> dict:
    """How far along a run is: agents, todos, notes and usage, from Strix's own state.

    These are the files Strix's viewer reads, so this is the terminal equivalent and it
    works while the scan is still writing them. `_run` lets a caller that already read the
    run hand that result in, so the SARIF is not parsed twice for one detail response.
    """
    run = _run if _run is not None else read_run(cwd, run_name)
    run_dir = Path(run["run_dir"])
    meta = _read_json(run_dir / "run.json", {})
    usage = meta.get("llm_usage") or {}
    agents = _read_json(run_dir / ".state" / "agents.json", {})
    todos = _read_json(run_dir / ".state" / "todos.json", {})
    notes = _read_json(run_dir / ".state" / "notes.json", {})

    by_status = agents.get("statuses") or {}
    names = agents.get("names") or {}
    todo_statuses = [t.get("status") for per in (todos or {}).values()
                     for t in (per or {}).values()]
    details = usage.get("input_tokens_details") or [{}]
    findings = run["vulnerabilities_count"]
    return {
        "run": run["run"],
        "dir": run["run_dir"],
        "status": run["status"],
        "start_time": meta.get("start_time"),
        "end_time": meta.get("end_time"),
        "turns": run["turns"],
        "cost_usd": run["cost_usd"],
        "findings": findings if findings is not None else len(run["findings"]),
        "coverage_gaps": len(run["coverage_gaps"]),
        "agents": {
            "total": len(by_status),
            "completed": sum(1 for s in by_status.values() if s == "completed"),
            "running": [names.get(a, a) for a, s in by_status.items() if s == "running"],
            "waiting": sum(1 for s in by_status.values() if s == "waiting"),
            "failed": sum(1 for s in by_status.values() if s in ("failed", "error")),
            "names": list(names.values()),
        },
        "todos": {
            "total": len(todo_statuses),
            "done": todo_statuses.count("done"),
            "in_progress": todo_statuses.count("in_progress"),
            "pending": todo_statuses.count("pending"),
        },
        "notes": len(notes or {}),
        "usage": {
            "requests": usage.get("requests"),
            "input_tokens": usage.get("input_tokens"),
            "cached_tokens": details[0].get("cached_tokens") if details else None,
            "output_tokens": usage.get("output_tokens"),
        },
    }


def _clamp(text, limit):
    """Trim a block of text to `limit` chars, reporting whether anything was cut."""
    if text is None:
        return "", False
    if not isinstance(text, str):
        text = json.dumps(text, default=str)
    text = text.strip()
    if len(text) > limit:
        return text[:limit], True
    return text, False


def _content_text(content):
    """Flatten assistant content into plain text.

    Strix stores this three ways depending on the row: a list of blocks with a `text`
    key, a bare string, or a string that is itself JSON of the blocks.
    """
    if content is None:
        return ""
    if isinstance(content, str):
        stripped = content.strip()
        if stripped[:1] in ("[", "{"):
            try:
                return _content_text(json.loads(stripped))
            except ValueError:
                return content
        return content
    if isinstance(content, list):
        parts = []
        for block in content:
            if isinstance(block, dict) and isinstance(block.get("text"), str):
                parts.append(block["text"])
            elif isinstance(block, str):
                parts.append(block)
        return "\n".join(parts)
    return str(content)


def _arguments_summary(raw):
    """A compact one-line view of a tool call's arguments, which arrive JSON-encoded."""
    if raw is None:
        return ""
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except ValueError:
            return raw
    if isinstance(raw, dict):
        return ", ".join(f"{k}={json.dumps(v, default=str) if isinstance(v, (dict, list)) else v}"
                         for k, v in raw.items())
    return json.dumps(raw, default=str)


def _read_messages(state_dir, names, limit):
    """The run's verbose agent stream, oldest first, from its own agents.db.

    Read read-only and defensively: a scan mid-write, or before its first row lands, must
    yield no messages rather than break the progress call that the dashboard polls.
    """
    limit = max(0, int(limit or 0))
    if limit == 0:
        return []
    db = Path(state_dir) / "agents.db"
    if not db.is_file():
        return []
    try:
        con = sqlite3.connect(f"file:{db}?mode=ro", uri=True, timeout=1.0)
        try:
            rows = con.execute(
                "select id, session_id, message_data, created_at from agent_messages "
                "order by id desc limit ?", (limit,)).fetchall()
        finally:
            con.close()
    except sqlite3.Error:
        return []

    out = []
    for mid, sid, raw, at in reversed(rows):
        try:
            data = json.loads(raw)
        except (ValueError, TypeError):
            continue
        if not isinstance(data, dict):
            continue
        mtype = data.get("type") or "message"
        entry = {"id": mid, "session_id": sid, "agent_name": names.get(sid, sid),
                 "role": data.get("role"), "type": mtype, "text": "", "tool": None,
                 "at": at, "truncated": False}
        if mtype == "function_call":
            entry["tool"] = data.get("name")
            entry["text"], entry["truncated"] = _clamp(_arguments_summary(data.get("arguments")), 1500)
        elif mtype == "function_call_output":
            entry["text"], entry["truncated"] = _clamp(data.get("output"), 800)
        elif mtype == "reasoning":
            entry["text"], entry["truncated"] = _clamp(
                _content_text(data.get("summary") or data.get("text")), 1500)
        else:
            entry["text"], entry["truncated"] = _clamp(_content_text(data.get("content")), 1500)
        out.append(entry)
    return out


def run_progress_detail(cwd, run_name=None, log_lines=200, messages=200) -> dict:
    """run_progress plus the detail a human wants: agents, todos, findings, coverage,
    notes and the log tail.

    The summary keys keep their order and shape; every extra key is appended, so callers
    polling the plain summary see no change.
    """
    run = read_run(cwd, run_name)
    out = run_progress(cwd, run_name, run)
    run_dir = Path(out["dir"])
    state = run_dir / ".state"

    meta = _read_json(run_dir / "run.json", {})
    meta = meta if isinstance(meta, dict) else {}

    agents = _read_json(state / "agents.json", {})
    agents = agents if isinstance(agents, dict) else {}
    names = agents.get("names") or {}
    pending_counts = agents.get("pending_counts") or {}
    out["agents_detail"] = [
        {"id": aid, "name": names.get(aid, aid), "status": status,
         "pending": pending_counts.get(aid, 0)}
        for aid, status in (agents.get("statuses") or {}).items()
    ]

    todos = _read_json(state / "todos.json", {})
    todos = todos if isinstance(todos, dict) else {}
    out["todos_detail"] = [
        {"agent_id": aid, "agent_name": names.get(aid, aid), "id": tid,
         "title": todo.get("title"), "status": todo.get("status")}
        for aid, per in todos.items()
        for tid, todo in (per or {}).items() if isinstance(todo, dict)
    ]

    # A missing threat-model file means "not applicable", not "zero", so only report a
    # count when the file actually parsed.
    threat = _read_json(state / "threat_models.json", None)
    if isinstance(threat, (dict, list)):
        out["threat_models"] = len(threat)

    vulns = _read_json(run_dir / "vulnerabilities.json", None)
    if isinstance(vulns, dict):
        vulns = vulns.get("vulnerabilities") or vulns.get("findings")
    if not isinstance(vulns, list) or not vulns:
        for key in ("findings", "vulnerabilities", "results"):
            if isinstance(meta.get(key), list) and meta[key]:
                vulns = meta[key]
                break
    out["findings_detail"] = [
        {"title": f.get("title") if isinstance(f, dict) else str(f),
         "severity": f.get("severity") if isinstance(f, dict) else None}
        for f in (vulns if isinstance(vulns, list) else [])
    ]

    # Coverage is split across files: the summary is Strix's own, and the gaps come from
    # run.json when it carries a list, else fall back to the SARIF-derived list read_run
    # already built -- the same list the summary count (`coverage_gaps`) is taken from, so
    # the count and this list cannot disagree. Report the key only when there is something.
    coverage = {}
    cov = _read_json(run_dir / "coverage.json", {})
    if isinstance(cov, dict) and cov.get("summary"):
        coverage["summary"] = cov["summary"]
    gaps = meta.get("coverage_gaps")
    if not isinstance(gaps, list):
        gaps = run["coverage_gaps"]
    if gaps:
        coverage["gaps"] = gaps
    if coverage:
        out["coverage"] = coverage

    try:
        lines = (run_dir / "strix.log").read_text(encoding="utf-8", errors="replace").splitlines()
        out["log_tail"] = lines[-log_lines:] if log_lines and log_lines > 0 else []
    except OSError:
        out["log_tail"] = []

    notes = _read_json(state / "notes.json", {})
    notes = notes if isinstance(notes, dict) else {}
    notes_detail = [
        {"id": nid, "title": n.get("title"), "agent_name": n.get("agent_name")}
        for nid, n in notes.items() if isinstance(n, dict)
    ]
    if notes_detail:
        out["notes_detail"] = notes_detail
    out["messages"] = _read_messages(state, names, messages)
    return out


def fix_viewer_config() -> str | None:
    """Make Strix's own config loadable, so `strix view` can start.

    Strix persists LLM_EXTRA_HEADERS as a JSON string, but types that field as a dict, so
    its settings model rejects the file and the viewer dies before serving anything. The
    scan path tolerates it; the viewer does not. Rewrite the value as an object when it is
    a string, and report it, so the change is never silent.
    """
    path = Path.home() / ".strix" / "cli-config.json"
    try:
        cfg = json.loads(path.read_text())
        raw = (cfg.get("env") or {}).get("LLM_EXTRA_HEADERS")
    except (OSError, ValueError, AttributeError):
        return None
    if not isinstance(raw, str):
        return None
    try:
        cfg["env"]["LLM_EXTRA_HEADERS"] = json.loads(raw)
    except ValueError:
        return None
    path.write_text(json.dumps(cfg, indent=2) + "\n")
    path.chmod(0o600)
    return (f"rewrote LLM_EXTRA_HEADERS in {path} as an object "
            "(Strix writes it as a string, which its own settings model rejects)")


def _sarif_location(res: dict) -> str:
    try:
        loc = res["locations"][0]["physicalLocation"]
        art = loc.get("artifactLocation", {}).get("uri", "")
        line = loc.get("region", {}).get("startLine")
        return f"{art}:{line}" if line else art
    except (KeyError, IndexError, TypeError):
        return ""


_SEV = {"critical": "critical", "high": "high", "medium": "medium",
        "low": "low", "info": "info", "informational": "info"}

_SARIF_LEVEL = {"error": "high", "warning": "medium", "note": "low", "none": "info"}


def _level_to_severity(level) -> str | None:
    return _SARIF_LEVEL.get(str(level).lower())


def normalise(f: dict) -> dict:
    """Map a Strix finding onto the internal handoff contract
    (contracts/finding.schema.json). Tolerant of shape drift across versions."""
    if not isinstance(f, dict):
        return {"title": str(f)}
    sev = _SEV.get(str(f.get("severity", "")).lower(), f.get("severity"))
    return {
        "id": f.get("id") or f.get("finding_id") or f.get("rule") or f.get("ruleId"),
        "title": f.get("title") or f.get("name") or f.get("message"),
        "vuln_class": f.get("category") or f.get("vuln_class") or f.get("cwe") or f.get("rule"),
        "severity": sev,
        "cvss": f.get("cvss") or f.get("cvss_score"),
        "target": f.get("target") or f.get("host") or f.get("url") or f.get("location"),
        "evidence": f.get("evidence") or f.get("proof_of_concept") or f.get("poc"),
        "description": f.get("description") or f.get("message"),
        "confidence": f.get("confidence"),
        "source_tool": "strix",
    }


def to_cairn_leads(run: dict):
    """Split a normalised run into (hints, intents) ready for the Cairn graph.

    One Hint per finding (situational awareness for the workers) and one Intent
    per finding worth acting on (from `origin`, unless a better anchor is known).
    """
    hints, intents = [], []
    for f in run.get("findings", []):
        title = f.get("title") or f.get("id") or "unnamed finding"
        if not (f.get("title") or f.get("id")):
            continue  # shape we cannot describe is not a lead
        sev = f.get("severity") or "unknown"
        line = f"[{sev}] {title}"
        if f.get("target"):
            line += f" @ {f['target']}"
        if f.get("evidence"):
            line += f" -- {str(f['evidence'])[:300]}"
        hints.append(line)
        if str(sev).lower() in ("critical", "high", "medium"):
            intents.append(f"Validate and attempt to exploit: {line}")
    for gap in run.get("coverage_gaps", [])[:20]:
        hints.append(f"[coverage-gap] {gap.get('message') or gap.get('rule')} "
                     f"({gap.get('location','')}) -- NOT a vulnerability, treat as unexamined")
    return hints, intents
