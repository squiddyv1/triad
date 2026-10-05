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
import subprocess
from pathlib import Path

COVERAGE_RULE_PREFIX = "strix-coverage/"

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
    return {"pid": proc.pid, "cwd": str(cwd), "cmd": cmd,
            "log": str(log), "note": "poll strix_runs/ for artifacts"}


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
