#!/usr/bin/env python3
"""triad: command-line driver for the Strix + Cairn stack.

This is the normal entry point, and it needs no agent framework: it loads the
`plugin/` package directly. Hermes, when installed, exposes that same package as
tools, so the engagement loop can equally run from a chat session.

Start here:

    triad                  where things stand; offers setup on a fresh checkout
    triad setup            prompts for API keys, writes .env, offers to start
    triad up               start the Cairn server and the dispatcher
    triad down             stop them (data is kept)

Then run an engagement:

    python3 triad.py engage  --title ACME --target https://app.example --goal "admin access" \
                             --roe contracts/roe-instructions.md
    python3 triad.py scan    --target https://app.example --roe contracts/roe-instructions.md \
                             --workdir ~/engagements/acme --mode quick --max-turns 50 --wait
    python3 triad.py feed    --project proj_001 --workdir ~/engagements/acme
    python3 triad.py watch   --project proj_001 --timeout 1800
    python3 triad.py report  --project proj_001 --workdir ~/engagements/acme -o report.md

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
WORKER_KEY_CHOICES = [
    ("skip: my workers reuse the host CLI's own login", None),
    ("Anthropic  -> ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_AUTH_TOKEN"),
    ("DeepSeek   -> DEEPSEEK_API_KEY", "DEEPSEEK_API_KEY"),
    ("OpenAI     -> OPENAI_API_KEY", "OPENAI_API_KEY"),
]
BIND_CHOICES = [
    ("127.0.0.1  loopback only (safer)", "127.0.0.1"),
    ("0.0.0.0    reachable from the LAN (convenient, note the exposure)", "0.0.0.0"),
]

_BOLD, _GRN, _YEL, _RED, _RST = "\033[1m", "\033[32m", "\033[33m", "\033[31m", "\033[0m"

# Returned by the prompts when the user bails out (Ctrl-C/EOF). Distinct from a
# None option value, because "skip this" is a real choice in more than one menu.
CANCELLED = object()


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


def _ask(question, default=""):
    """One prompt. Returns the default on empty input, CANCELLED if the user bails."""
    suffix = f" [{default}]" if default else ""
    try:
        answer = input(f"  {question}{suffix}: ").strip()
    except (EOFError, KeyboardInterrupt):
        print()
        return CANCELLED
    return answer or default


def _ask_secret(question):
    """Prompt without echoing.

    getpass needs a readable controlling terminal; under a pipe or some CI
    wrappers it raises instead of blocking. Falling back to a visible prompt
    keeps the wizard usable there, rather than aborting setup.
    """
    try:
        import getpass
        return getpass.getpass(f"  {question}: ").strip()
    except KeyboardInterrupt:
        print()
        return CANCELLED
    except Exception:
        print("  (no hidden prompt available here; what you type will be visible)")
        return _ask(question)


def _choose(question, options, default=1):
    """Numbered menu. Returns the option's value, CANCELLED if the user bails."""
    print(f"  {question}")
    for index, (label, _value) in enumerate(options, 1):
        mark = "  (default)" if index == default else ""
        print(f"    {index}. {label}{mark}")
    while True:
        raw = _ask("Choose", str(default))
        if raw is CANCELLED:
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


def _docker_ok():
    if not shutil.which("docker"):
        return False
    try:
        return subprocess.run(["docker", "info"], stdout=subprocess.DEVNULL,
                              stderr=subprocess.DEVNULL, timeout=20).returncode == 0
    except (OSError, subprocess.SubprocessError):
        return False


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
        return None, "uv not found; install it (https://docs.astral.sh/uv/) or start Docker"
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
        return None, "uv not found; install it (https://docs.astral.sh/uv/) or start Docker"
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
        if model is CANCELLED:
            return _aborted()
        if model == "__other__":
            model = _ask("  Model id (LiteLLM form, e.g. openrouter/z-ai/glm-5.3)")
            if model is CANCELLED:
                return _aborted()
        if model:
            updates["STRIX_LLM"] = model

        key = _ask_secret("API key for that provider (LLM_API_KEY, hidden)")
        if key is CANCELLED:
            return _aborted()
        if key:
            updates["LLM_API_KEY"] = key
        elif not current.get("LLM_API_KEY"):
            _warn("no key entered; Strix scans will fail until LLM_API_KEY is set")

        # Only the container dispatcher needs these: in local mode the workers
        # reuse whatever the host worker CLI is already logged into.
        worker_key = _choose("Worker LLM key for the Cairn dispatcher?", WORKER_KEY_CHOICES, default=1)
        if worker_key is CANCELLED:
            return _aborted()
        if worker_key:
            value = _ask_secret(f"{worker_key} (hidden)")
            if value is CANCELLED:
                return _aborted()
            if value:
                updates[worker_key] = value

        bind = _choose("Where should the Cairn API and console bind?", BIND_CHOICES, default=1)
        if bind is CANCELLED:
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

    if interactive:
        answer = _ask("Start Cairn and the dispatcher now? (Y/n)", "y")
        if answer is CANCELLED:
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

    if _cairn_up():
        _ok(f"Cairn already answering at {_base_url()}")
    elif getattr(args, "container", False):
        compose = _compose_cmd()
        if compose is None or not _docker_ok():
            _err("--container needs a running Docker with the compose plugin")
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
    elif _docker_ok() and _compose_cmd():
        _ok("starting cairn-server with docker compose (first run builds the image)")
        if subprocess.run(_compose_cmd() + ["up", "-d", "--build", "cairn-server"],
                          cwd=str(REPO)).returncode != 0:
            _err("docker compose failed; see the output above")
            return 2
        if not _wait_for_cairn():
            _err("Cairn did not come up; check: docker compose logs cairn-server")
            return 2
    else:
        _warn("Docker is not usable here, so the Cairn server runs as a host process")
        _warn("that is the no-sandbox path: workers inherit your user's permissions")
        pid, problem = _server_local_start()
        if problem:
            _err(problem)
            return 2
        _ok(f"cairn serve started (pid {pid}, log {SERVER_LOG})")
        if not _wait_for_cairn(timeout=90):
            _err(f"Cairn did not answer; see {SERVER_LOG}")
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
            _warn("no worker CLI found (opencode, claude, codex, pi)")
            _warn("the dispatcher needs one to claim intents; install one, or use the")
            _warn("containerised dispatcher with dispatch.yaml on an amd64 host")
        else:
            _ok(f"worker CLI: {worker} ({where})")
        config = getattr(args, "config", None) or "dispatch.local.yaml"
        pid, problem = _dispatcher_start(config)
        if problem:
            _err(f"the dispatcher did not start: {problem}")
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
