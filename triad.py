#!/usr/bin/env python3
"""triad: command-line driver for the Strix + Cairn stack.

This is the normal entry point, and it needs no agent framework: it loads the
`plugin/` package directly. Hermes, when installed, exposes that same package as
tools, so the engagement loop can equally run from a chat session.

Start here:

    triad                  where things stand; offers setup on a fresh checkout
    triad setup            prompts for API keys, writes .env, offers to start
    triad auth             write the worker CLI's credentials from .env
    triad up               start the Cairn server and the dispatcher
    triad down             stop them (data is kept)

Then run an engagement. `engage` is the whole default flow: it creates the project,
runs the Strix scan, then feeds the findings into the graph as hints and intents.

    triad engage --title ACME --target https://app.example \
                 --goal "conclude or rule out every finding in scope" \
                 --roe contracts/roe-instructions.md
    triad watch  --project proj_001
    triad report --project proj_001 --workdir ~/engagements/acme -o report.md

The steps are also usable on their own, which is what --no-scan and the individual
commands are for when a scan is already running or was run elsewhere:

    triad engage --no-scan ...                       # project only
    triad scan   --target ... --workdir ...          # Strix only
    triad findings --workdir ...                     # read a run
    triad feed   --project proj_001 --workdir ...    # feed an existing run

`triad up` runs the dispatcher as a host process, which is the path verified end
to end here; the containerised dispatcher needs the amd64-only worker image, so
local mode is the default on every architecture. `triad down` stops both.
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



def _slug(text, fallback="engagement"):
    """A filesystem-safe name for an engagement directory."""
    cleaned = re.sub(r"[^a-z0-9]+", "-", (text or "").lower()).strip("-")
    return cleaned[:48] or fallback


def _engage_workdir(args):
    """Where this engagement's Strix run and artifacts live.

    Defaults under TRIAD_WORKDIR (set by the installed wrapper and by .env) so the
    layout matches what the docs and the plugin already assume.
    """
    if getattr(args, "workdir", None):
        return Path(args.workdir).expanduser().resolve()
    base = (os.environ.get("TRIAD_WORKDIR") or _env_read().get("TRIAD_WORKDIR")
            or "~/engagements")
    return (Path(base).expanduser() / _slug(getattr(args, "title", ""))).resolve()


def _strix_available():
    if shutil.which("strix"):
        return True
    return (Path.home() / ".strix" / "bin" / "strix").exists()


def _feed_run(c, project, workdir, run_id=None, anchor="origin"):
    """Post one Strix run into the graph.

    A hint per finding, an intent per actionable finding, all anchored on the
    origin fact. This is the handoff the whole tool exists for: without it the
    graph has nothing to search and the scan is just JSON on disk.
    """
    run = strix.read_run(Path(workdir).expanduser(), run_id)
    hints, intents = strix.to_cairn_leads(run)
    posted_hints, posted_intents = [], []
    for h in hints:
        posted_hints.append(c.add_hint(project, f"[strix] {h}", "hermes.strix").get("id"))
    for d in intents:
        posted_intents.append(c.add_intent(project, [anchor], d, "hermes.strix").get("id"))
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

    # Cairn has to wait for the scan. The dispatcher would otherwise begin a
    # bootstrap pass and claim intents the moment the project exists, on a graph
    # holding none of Strix's input, which duplicates the scan and spends budget on
    # the wrong work. Stop it before the project exists, not after: the window
    # between create and scan is exactly when it would start.
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

    if not _strix_available():
        _err("strix is not installed, so nothing can be fed into the graph")
        print("     ./install.sh installs it; the project above still exists")
        return 2

    workdir = _engage_workdir(args)
    summary["workdir"] = str(workdir)
    print(f"\n1/2 strix {args.mode} scan (max {args.max_turns} turns) -> {workdir}")
    launch = strix.run_scan(args.target, workdir, instruction_file=args.roe,
                            scan_mode=args.mode, max_turns=args.max_turns)
    print(f"    pid {launch['pid']}; log {launch['log']}")
    print(f"    waiting up to {args.scan_timeout}s for the run to settle")
    run_dir = _wait_for_run(workdir, timeout=args.scan_timeout)
    if run_dir is None:
        _err("no Strix run directory appeared, so there is nothing to feed")
        if held:
            _warn("cairn stays paused, because nothing was fed to it")
            print("     start it on the unfed graph with:  triad up")
        print(f"     check {launch['log']}, then:  triad feed --project {pid} --workdir {workdir}")
        return 1

    print(f"    run: {run_dir.name}")
    print(f"\n2/2 feeding it into {pid}")
    run, posted_hints, posted_intents = _feed_run(c, pid, workdir, None, args.anchor)
    summary.update({"findings": len(run["findings"]),
                    "coverage_gaps": len(run["coverage_gaps"]),
                    "hints_posted": len(posted_hints),
                    "intents_posted": len(posted_intents)})
    print(f"    findings {len(run['findings'])}  coverage gaps {len(run['coverage_gaps'])}")
    print(f"    hints {len(posted_hints)}  intents {len(posted_intents)}")
    if not run["findings"]:
        _warn("the scan found nothing, so the graph gained only coverage gaps")
    if run.get("status") in ("running", "in_progress", None):
        _warn("that run had not finished; feeding it again later is safe (hints and")
        _warn("intents are additive, so re-run: triad feed --project ... --workdir ...)")

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
    run, posted_hints, posted_intents = _feed_run(
        client(), args.project, args.workdir, args.run, args.anchor)
    print(f"fed run {run['run']} -> {args.project}")
    print(f"  findings: {len(run['findings'])}  gaps: {len(run['coverage_gaps'])}")
    print(f"  hints posted:   {posted_hints}")
    print(f"  intents posted: {posted_intents}")
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



# env, stack control and the first-run wizard
#
# The pieces below turn the installer's "next steps" list into something the CLI
# does itself: collect keys into .env, bring Cairn up, start the dispatcher and
# report readiness. Runtime state lives in .triad/ (gitignored) so a broken run
# can be inspected or deleted without touching anything else.

STATE_DIR = REPO / ".triad"
SERVER_PID = STATE_DIR / "server.pid"
DISPATCH_PID = STATE_DIR / "dispatcher.pid"
SERVER_LOG = STATE_DIR / "server.log"
DISPATCH_LOG = STATE_DIR / "dispatcher.log"
CAIRN_DIR = REPO / "cairn"
DEFAULT_BASE = "http://127.0.0.1:8000"
WORKER_CLIS = ("opencode", "claude", "codex", "pi")

# LiteLLM accepts provider/model, so the wizard offers the common ones and a free
# text escape hatch. The first entry is this repo's documented default.
MODEL_CHOICES = [
    ("OpenRouter  openrouter/z-ai/glm-5.3  (this repo's default)", "openrouter/z-ai/glm-5.3"),
    ("DeepSeek    deepseek/deepseek-chat", "deepseek/deepseek-chat"),
    ("Anthropic   anthropic/claude-sonnet-4-5", "anthropic/claude-sonnet-4-5"),
    ("Other       type any provider/model id", "__other__"),
]
# The bundled worker is opencode, so opencode's own providers come first: those get
# written into its credentials file. The rest are read only by the containerised
# workers in dispatch.yaml.
WORKER_KEY_CHOICES = [
    ("OpenCode Go / Zen -> OPENCODE_GO_API_KEY    (drives the bundled worker)", "OPENCODE_GO_API_KEY"),
    ("OpenRouter        -> OPENROUTER_API_KEY     (also usable by the worker)", "OPENROUTER_API_KEY"),
    ("skip               (add keys to .env yourself later)", None),
    ("DeepSeek          -> DEEPSEEK_API_KEY    (container workers only)", "DEEPSEEK_API_KEY"),
    ("Anthropic         -> ANTHROPIC_AUTH_TOKEN (container workers only)", "ANTHROPIC_AUTH_TOKEN"),
    ("OpenAI            -> OPENAI_API_KEY     (container workers only)", "OPENAI_API_KEY"),
]
BIND_CHOICES = [
    ("127.0.0.1  loopback only (safer)", "127.0.0.1"),
    ("0.0.0.0    reachable from the LAN (convenient, note the exposure)", "0.0.0.0"),
]

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
    print(f"  {_colour('✗', _RED)} {text}", file=sys.stderr)


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
    """Prompt for one line, with our own line editing.

    Deliberately not `input` or `getpass`: both depend on the terminal's line
    discipline, which a parent process can leave switched off. In that state backspace
    is not an edit, it is a byte, so the value arrives holding control characters and
    the display shows ^H, while Ctrl-C does not interrupt. Reading the bytes ourselves
    means one backspace press deletes one character whatever the terminal is set to,
    and no control byte can become part of a key.

    Returns the line, or CANCELLED when the user interrupts or the input ends.
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
    # ISIG is cleared as well: with it on, Ctrl-C is turned into a signal by the
    # driver and never reaches us as a byte. Signals depend on this process being in
    # the terminal's foreground group, which is not guaranteed (it fails outright in
    # a pty whose foreground group is someone else, and Ctrl-C then does nothing at
    # all). Reading the byte and cancelling ourselves works either way.
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


def _worker_cli():
    for name in WORKER_CLIS:
        found = shutil.which(name)
        if found:
            return name, found
    home_opencode = Path.home() / ".opencode" / "bin" / "opencode"
    if home_opencode.is_file():
        return "opencode", str(home_opencode)
    return None, None


# Why Docker is unusable, so `triad up` can name the actual problem instead of
# saying "Docker is not usable" and leaving the user to guess. Installed-but-broken
# is the common case: the daemon is down, or the user is not in the docker group yet.
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
    return _dispatcher_start(config or "dispatch.local.yaml")


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
    if interactive:
        model = _choose("Which model should Strix drive?", MODEL_CHOICES, default=1)
        if isinstance(model, _Cancelled):
            return _aborted()
        if model == "__other__":
            model = _ask("  Model id (LiteLLM form, e.g. openrouter/z-ai/glm-5.3)")
            if isinstance(model, _Cancelled):
                return _aborted()
        if model:
            updates["STRIX_LLM"] = model

        key = _ask_secret("API key for that provider (LLM_API_KEY, hidden)")
        if isinstance(key, _Cancelled):
            return _aborted()
        if key:
            updates["LLM_API_KEY"] = key
        elif not current.get("LLM_API_KEY"):
            _warn("no key entered; Strix scans will fail until LLM_API_KEY is set")

        # Only the container dispatcher needs these: in local mode the workers
        # reuse whatever the host worker CLI is already logged into.
        worker_key = _choose("Worker LLM key for the Cairn dispatcher?", WORKER_KEY_CHOICES, default=1)
        if isinstance(worker_key, _Cancelled):
            return _aborted()
        if worker_key:
            value = _ask_secret(f"{worker_key} (hidden)")
            if isinstance(value, _Cancelled):
                return _aborted()
            if value:
                updates[worker_key] = value

        bind = _choose("Where should the Cairn API and console bind?", BIND_CHOICES, default=1)
        if isinstance(bind, _Cancelled):
            return _aborted()
        if bind:
            updates["CAIRN_BIND"] = bind
    else:
        updates["STRIX_LLM"] = current.get("STRIX_LLM") or MODEL_CHOICES[0][1]

    updates.setdefault("CAIRN_BASE_URL", current.get("CAIRN_BASE_URL") or DEFAULT_BASE)
    updates.setdefault("TRIAD_WORKDIR", current.get("TRIAD_WORKDIR") or "~/engagements")
    updates.setdefault("STRIX_TELEMETRY", current.get("STRIX_TELEMETRY") or "0")

    path = _env_write(updates)
    _hdr("Written")
    _ok(f"{path} (mode 600)")
    merged = _env_read()
    _ok(f"STRIX_LLM        {merged.get('STRIX_LLM') or '(unset)'}")
    _ok(f"LLM_API_KEY      {_mask(merged.get('LLM_API_KEY'))}")
    for name in ("ANTHROPIC_AUTH_TOKEN", "DEEPSEEK_API_KEY", "OPENAI_API_KEY"):
        if merged.get(name):
            _ok(f"{name:16} {_mask(merged[name])}")
    _ok(f"CAIRN_BIND       {merged.get('CAIRN_BIND') or '(compose default)'}")
    _ok(f"CAIRN_BASE_URL   {merged.get('CAIRN_BASE_URL')}")

    # opencode reads its own credentials file, so anything given above goes straight
    # in. This is what makes a separate interactive `opencode auth login` unnecessary.
    worker_entries = {provider: merged[var]
                      for var, provider in OPENCODE_AUTH_KEYS.items() if merged.get(var)}
    if worker_entries:
        auth_path = _opencode_auth_set(worker_entries)
        for provider in sorted(worker_entries):
            _ok(f"opencode credentials: {provider} -> {auth_path}")

    if interactive:
        answer = _ask("Start Cairn and the dispatcher now? (Y/n)", "y")
        if isinstance(answer, _Cancelled):
            return _aborted()
        if answer.lower() not in ("n", "no"):
            return cmd_up(args)
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
        config = getattr(args, "config", None) or "dispatch.local.yaml"
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


def cmd_down(args):
    """Stop the dispatcher and the Cairn server. Data is kept."""
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
    if _docker_ok():
        compose = _compose_cmd()
        if compose:
            subprocess.run(compose + ["stop", "cairn-server", "cairn-dispatcher"],
                           cwd=str(REPO), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _ok("data kept in ./datas/cairn; use 'make down' to remove the containers too")
    return 0


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


def cmd_home(args):
    """Bare `triad`: say where things stand, and offer setup if they are not."""
    print(f"{_colour('triad', _BOLD)}  Strix (discovery) + Cairn (exploitation)")
    env = _env_read()
    configured = bool(env.get("LLM_API_KEY"))
    if not configured:
        _warn("not configured yet" if env else f"no {_env_path()} yet")
        if sys.stdin.isatty():
            print("  Running the setup wizard.")
            return cmd_setup(args)
        print("  Run:  triad setup")
        return 0

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
    ap = argparse.ArgumentParser(prog="triad", description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd")

    su = sub.add_parser("setup", help="interactive first run: keys, then start the stack")
    su.add_argument("--yes", action="store_true", help="take every default, prompt nothing")
    su.set_defaults(func=cmd_setup)

    up = sub.add_parser("up", help="start the Cairn server and the dispatcher")
    up.add_argument("--config", help="dispatcher config (default: dispatch.local.yaml)")
    up.add_argument("--container", action="store_true",
                    help="use the compose dispatcher instead of the host one (amd64 only)")
    up.set_defaults(func=cmd_up)

    dn = sub.add_parser("down", help="stop the dispatcher and the Cairn server")
    dn.add_argument("--keep-server", action="store_true", help="stop only the dispatcher")
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
