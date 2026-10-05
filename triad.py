#!/usr/bin/env python3
"""triad: command-line driver for the Strix + Cairn stack.

This is the normal entry point, and it needs no agent framework: it loads the
`plugin/` package directly. Hermes, when installed, exposes that same package as
tools, so the engagement loop can equally run from a chat session.

    python3 triad.py engage  --title ACME --target https://app.example --goal "admin access" \
                             --roe contracts/roe-instructions.md
    python3 triad.py scan    --target https://app.example --roe contracts/roe-instructions.md \
                             --workdir ~/engagements/acme --mode quick --max-turns 50 --wait
    python3 triad.py feed    --project proj_001 --workdir ~/engagements/acme
    python3 triad.py watch   --project proj_001 --timeout 1800
    python3 triad.py report  --project proj_001 --workdir ~/engagements/acme -o report.md

The Cairn dispatcher is a separate process (it owns the workers):

    uv run --project cairn cairn dispatch --config dispatch.local.yaml
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys
import time
import types
from pathlib import Path

def _repo_root() -> Path:
    """Locate the harness root.

    Order: $TRIAD_HOME, then upward from this file. Deliberately does not look
    under $HERMES_HOME: the normal flow is Strix -> Cairn and has no Hermes
    dependency, so nothing here should require a Hermes install to exist.
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



def cmd_engage(args):
    hints = _read_roe(args.roe) or []
    res = client().create_project(
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
    if args.json:
        print(json.dumps(res))
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
        print("  (not waiting; poll with: triad.py findings --workdir ...)")
        return 0
    print("  waiting for the run directory to appear and settle...")
    run_dir = _wait_for_run(workdir, timeout=args.wait_timeout)
    if run_dir is None:
        print("  !! no run directory appeared in time")
        return 1
    print(f"  run dir: {run_dir}")
    return 0


def _wait_for_run(workdir, timeout):
    deadline = time.time() + timeout
    last = None
    while time.time() < deadline:
        d = strix.latest_run_dir(workdir)
        if d:
            last = d
            if (d / "run.json").is_file():
                try:
                    status = json.loads((d / "run.json").read_text()).get("status")
                except (json.JSONDecodeError, OSError):
                    status = None
                if status and status not in ("running", "in_progress", None):
                    return d
            elif (d / "vulnerabilities.json").is_file() or (d / "findings.sarif").is_file():
                return d
        time.sleep(10)
    return last


def cmd_findings(args):
    run = strix.read_run(Path(args.workdir).expanduser(), args.run)
    print(f"run: {run['run']}  status={run['status']}  cost=${run['cost_usd']}  turns={run['turns']}")
    print(f"findings: {len(run['findings'])}   coverage gaps: {len(run['coverage_gaps'])}")
    for f in run["findings"]:
        print(f"  [{f.get('severity')}] {f.get('title')} @ {f.get('target')}")
    for g in run["coverage_gaps"]:
        print(f"  [gap] {g.get('message') or g.get('rule')}")
    return 0


def cmd_feed(args):
    c = client()
    run = strix.read_run(Path(args.workdir).expanduser(), args.run)
    hints, intents = strix.to_cairn_leads(run)
    posted = {"hints": [], "intents": []}
    for h in hints:
        posted["hints"].append(c.add_hint(args.project, f"[strix] {h}", "hermes.strix").get("id"))
    for d in intents:
        posted["intents"].append(
            c.add_intent(args.project, [args.anchor], d, "hermes.strix").get("id"))
    print(f"fed run {run['run']} -> {args.project}")
    print(f"  findings: {len(run['findings'])}  gaps: {len(run['coverage_gaps'])}")
    print(f"  hints posted:   {posted['hints']}")
    print(f"  intents posted: {posted['intents']}")
    return 0


def cmd_watch(args):
    c = client()
    deadline = time.time() + args.timeout
    last_sig = None
    while time.time() < deadline:
        g = c.get_project(args.project)
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
        time.sleep(args.interval)
    print(f"timed out after {args.timeout}s; last state: {last_sig}")
    return 1


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



def main(argv=None):
    ap = argparse.ArgumentParser(prog="triad", description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)

    e = sub.add_parser("engage", help="create a Cairn project for a target")
    e.add_argument("--title", required=True)
    e.add_argument("--target", required=True)
    e.add_argument("--goal", required=True)
    e.add_argument("--roe")
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

    args = ap.parse_args(argv)
    try:
        return args.func(args)
    except cairn.CairnError as exc:
        base = os.environ.get("CAIRN_BASE_URL", cairn.DEFAULT_BASE)
        if exc.status is None:
            print(f"error: Cairn is not answering at {base}\n       {exc}", file=sys.stderr)
            print("       start it with:  make up   (or: docker compose up -d)", file=sys.stderr)
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
